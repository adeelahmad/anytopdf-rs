use crate::chat::{
    AttachmentRef, ChatOptions, Conversation, Message, imessage, slack, telegram, whatsapp,
};
use crate::containers::{
    CopyLimit, MemberFile, copy_capped, import_members, member_dir, member_path,
};
use anyhow::{Context, Result};
use anytopdf_core::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Chat exports from WhatsApp, Telegram, Slack and iMessage, as files, export
/// folders or zip archives. Each conversation becomes text pages with one
/// line per message (time, sender, text) under date headings; attachments
/// that ship inside the export are imported inline, right after the message
/// that sent them, through whichever importer handles them.
pub struct ChatImporter {
    opts: ChatOptions,
    imported: ImportedAttachments,
}

impl ChatImporter {
    pub fn new(opts: ChatOptions, imported: ImportedAttachments) -> Self {
        Self { opts, imported }
    }
}

/// Files an export folder's chat imported as attachments, per job workspace.
/// When the whole folder is converted those files are also inputs of their
/// own; [`ChatAttachmentDedupe`] drops those duplicate sources.
#[derive(Debug, Default, Clone)]
pub struct ImportedAttachments(Arc<Mutex<HashMap<PathBuf, HashSet<PathBuf>>>>);

impl ImportedAttachments {
    fn record(&self, workspace: &Path, file: PathBuf) {
        if let Ok(mut map) = self.0.lock() {
            map.entry(workspace.to_path_buf()).or_default().insert(file);
        }
    }

    fn take(&self, workspace: &Path) -> HashSet<PathBuf> {
        self.0
            .lock()
            .ok()
            .and_then(|mut map| map.remove(workspace))
            .unwrap_or_default()
    }
}

/// Removes inputs that a chat export in the same run already imported inline
/// as attachments, so converting an export folder shows each photo once.
pub struct ChatAttachmentDedupe {
    imported: ImportedAttachments,
}

impl ChatAttachmentDedupe {
    pub fn new(imported: ImportedAttachments) -> Self {
        Self { imported }
    }
}

impl Plugin for ChatAttachmentDedupe {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "chat-attachments".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "graph-enricher".into(),
            extensions: Vec::new(),
            mime_types: Vec::new(),
            priority: 0,
        }
    }
}

impl GraphEnricher for ChatAttachmentDedupe {
    fn enrich_graph(&self, ctx: &JobContext, graph: &mut DocumentGraph) -> Result<Vec<String>> {
        let files = self.imported.take(&ctx.workspace);
        if files.is_empty() {
            return Ok(Vec::new());
        }
        let duplicates: HashSet<Uuid> = graph
            .sources
            .iter()
            .filter(|s| files.contains(&s.path))
            .map(|s| s.id)
            .collect();
        graph.sources.retain(|s| !duplicates.contains(&s.id));
        graph.units.retain(|u| !duplicates.contains(&u.source_id));
        Ok(Vec::new())
    }
}

const SNIFF_BYTES: u64 = 64 * 1024;
/// Largest chat file (or zip entry holding one) that is parsed.
pub const MAX_CHAT_BYTES: u64 = 512 * 1024 * 1024;
/// Largest single attachment imported.
pub const MAX_ATTACHMENT_BYTES: u64 = 512 * 1024 * 1024;
/// Attachments imported per input; later ones stay named in the text.
pub const MAX_CHAT_ATTACHMENTS: usize = 2_000;
/// A text unit (one chunk) is closed at the next message after this many bytes.
const UNIT_TEXT_BYTES: usize = 12 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 200;
const RATIO_FLOOR: u64 = 1024 * 1024;
const MAX_ZIP_ENTRIES: usize = 50_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    WhatsApp,
    Telegram,
    IMessage,
    SlackDay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Detected {
    File(Format),
    /// A single chat file inside a zip, with its attachments beside it.
    Zip {
        entry: String,
        format: Format,
    },
    /// A Slack workspace export; `root` is the folder prefix holding
    /// `users.json` (usually empty).
    SlackZip {
        root: String,
    },
}

fn read_prefix(reader: impl Read) -> Vec<u8> {
    let mut prefix = Vec::new();
    let _ = reader.take(SNIFF_BYTES).read_to_end(&mut prefix);
    prefix
}

fn sniff_text(prefix: &[u8], name: &str) -> Option<Format> {
    if prefix.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(prefix);
    if whatsapp::looks_like(&text) {
        Some(Format::WhatsApp)
    } else if imessage::looks_like(&text) {
        Some(Format::IMessage)
    } else if telegram::looks_like(&text) {
        Some(Format::Telegram)
    } else if slack::is_day_file_name(name) && slack::looks_like_day(&text) {
        Some(Format::SlackDay)
    } else {
        None
    }
}

fn file_name(name: &str) -> &str {
    name.rsplit(['/', '\\']).next().unwrap_or(name)
}

fn parent_prefix(name: &str) -> &str {
    name.rfind('/').map_or("", |i| &name[..=i])
}

fn open_zip(path: &Path) -> Option<zip::ZipArchive<BufReader<fs::File>>> {
    zip::ZipArchive::new(BufReader::new(fs::File::open(path).ok()?)).ok()
}

fn detect_zip(path: &Path) -> Option<Detected> {
    let mut archive = open_zip(path)?;
    let names: Vec<String> = archive
        .file_names()
        .take(MAX_ZIP_ENTRIES)
        .map(str::to_string)
        .collect();
    for name in &names {
        if file_name(name) == "users.json" {
            let root = parent_prefix(name);
            if names.iter().any(|n| *n == format!("{root}channels.json")) {
                return Some(Detected::SlackZip { root: root.into() });
            }
        }
    }
    let mut candidates: Vec<&String> = names
        .iter()
        .filter(|n| {
            let base = file_name(n);
            !n.contains("__MACOSX")
                && (base == "_chat.txt"
                    || base == "result.json"
                    || (base.starts_with("WhatsApp") && base.ends_with(".txt")))
        })
        .collect();
    candidates.sort_by_key(|n| n.matches('/').count());
    for name in candidates.into_iter().take(4) {
        let Ok(entry) = archive.by_name(name) else {
            continue;
        };
        if let Some(format) = sniff_text(&read_prefix(entry), file_name(name)) {
            return Some(Detected::Zip {
                entry: name.clone(),
                format,
            });
        }
    }
    None
}

fn detect(path: &Path) -> Option<Detected> {
    let prefix = read_prefix(fs::File::open(path).ok()?);
    if prefix.starts_with(b"PK\x03\x04") || prefix.starts_with(b"PK\x05\x06") {
        return detect_zip(path);
    }
    sniff_text(&prefix, &anytopdf_core::basename(path)).map(Detected::File)
}

fn read_capped(reader: impl Read, what: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_CHAT_BYTES + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("read {what}"))?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_CHAT_BYTES,
        "{what} is larger than {MAX_CHAT_BYTES} bytes"
    );
    Ok(bytes)
}

fn decode(bytes: Vec<u8>, name: &str, warnings: &mut Vec<String>) -> String {
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(e) => {
            warnings.push(
                Diagnostic::new(
                    DiagnosticCode::LossyDecode,
                    format!("{name} is not valid UTF-8; invalid bytes were replaced"),
                )
                .to_string(),
            );
            String::from_utf8_lossy(e.as_bytes()).into_owned()
        }
    }
}

fn file_stem(name: &str) -> String {
    let base = file_name(name);
    base.rsplit_once('.')
        .map_or(base, |(stem, _)| stem)
        .to_string()
}

/// Parses a single-file export. `name` is the chat file's own name and
/// `container` the zip or file name the title falls back to.
fn parse_format(
    format: Format,
    bytes: Vec<u8>,
    name: &str,
    container: &str,
    opts: &ChatOptions,
    directory: Option<&slack::Directory>,
    warnings: &mut Vec<String>,
) -> Result<Vec<Conversation>> {
    Ok(match format {
        Format::Telegram => telegram::parse(&bytes)?,
        Format::WhatsApp => {
            let title_source = if name == "_chat.txt" { container } else { name };
            let text = decode(bytes, name, warnings);
            vec![whatsapp::parse(
                &text,
                &whatsapp::title_from_name(title_source),
                opts.date_order,
            )]
        }
        Format::IMessage => {
            let text = decode(bytes, name, warnings);
            vec![imessage::parse(&text, &file_stem(name))]
        }
        Format::SlackDay => {
            let empty = slack::Directory::default();
            let directory = directory.unwrap_or(&empty);
            let messages = slack::parse_day(&bytes, directory)?;
            vec![slack::conversation(
                directory.title(container),
                vec![messages],
            )]
        }
    })
}

/// Reads the `users.json` and conversation lists of the Slack export that
/// holds `day_file` (`<export>/<channel>/<date>.json`).
fn slack_directory_beside(day_file: &Path, warnings: &mut Vec<String>) -> slack::Directory {
    let mut directory = slack::Directory::default();
    let Some(export) = day_file.parent().and_then(Path::parent) else {
        return directory;
    };
    let read = |name: &str| -> Option<Vec<u8>> {
        let path = export.join(name);
        fs::File::open(&path)
            .ok()
            .and_then(|f| read_capped(f, name).ok())
    };
    if let Some(bytes) = read("users.json")
        && let Err(e) = directory.add_users(&bytes)
    {
        warnings.push(Diagnostic::new(DiagnosticCode::ImportFailed, format!("{e:#}")).to_string());
    }
    for (name, prefix) in SLACK_LISTS {
        if let Some(bytes) = read(name) {
            let _ = directory.add_conversations(&bytes, prefix);
        }
    }
    directory
}

const SLACK_LISTS: [(&str, &str); 4] = [
    ("channels.json", "#"),
    ("groups.json", "#"),
    ("mpims.json", ""),
    ("dms.json", ""),
];

fn read_entry(archive: &mut zip::ZipArchive<BufReader<fs::File>>, name: &str) -> Result<Vec<u8>> {
    let entry = archive
        .by_name(name)
        .with_context(|| format!("open {name} in archive"))?;
    read_capped(entry, name)
}

fn parse_slack_zip(
    path: &Path,
    root: &str,
    warnings: &mut Vec<String>,
) -> Result<Vec<Conversation>> {
    let mut archive = open_zip(path).context("read zip archive")?;
    let mut directory = slack::Directory::default();
    directory.add_users(&read_entry(&mut archive, &format!("{root}users.json"))?)?;
    for (name, prefix) in SLACK_LISTS {
        let full = format!("{root}{name}");
        if archive.index_for_name(&full).is_some() {
            directory.add_conversations(&read_entry(&mut archive, &full)?, prefix)?;
        }
    }
    let mut folders: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let names: Vec<String> = archive
        .file_names()
        .take(MAX_ZIP_ENTRIES)
        .map(str::to_string)
        .collect();
    for name in names {
        let Some(rest) = name.strip_prefix(root) else {
            continue;
        };
        if let Some((folder, day)) = rest.split_once('/')
            && slack::is_day_file_name(day)
        {
            folders.entry(folder.to_string()).or_default().push(name);
        }
    }
    let mut conversations = Vec::new();
    for (folder, mut days) in folders {
        days.sort();
        let mut parsed = Vec::new();
        for day in days {
            match read_entry(&mut archive, &day).and_then(|b| slack::parse_day(&b, &directory)) {
                Ok(messages) => parsed.push(messages),
                Err(e) => warnings.push(
                    Diagnostic::new(
                        DiagnosticCode::MembersNotImported,
                        format!("{day} was not imported: {e:#}"),
                    )
                    .to_string(),
                ),
            }
        }
        conversations.push(slack::conversation(directory.title(&folder), parsed));
    }
    anyhow::ensure!(
        !conversations.is_empty(),
        "Slack export has no conversations"
    );
    Ok(conversations)
}

/// Where attachments named by an export are looked up.
enum Store {
    None,
    /// The export's folder; nothing outside it is read.
    Dir(PathBuf),
    /// A zip, with the folder prefix of the chat file inside it.
    Zip {
        archive: Box<zip::ZipArchive<BufReader<fs::File>>>,
        base: String,
        by_name: HashMap<String, Option<String>>,
    },
}

impl Store {
    fn zip(path: &Path, base: &str) -> Result<Self> {
        let archive = open_zip(path).context("read zip archive")?;
        let mut by_name: HashMap<String, Option<String>> = HashMap::new();
        for name in archive.file_names().take(MAX_ZIP_ENTRIES) {
            by_name
                .entry(file_name(name).to_string())
                .and_modify(|v| *v = None)
                .or_insert_with(|| Some(name.to_string()));
        }
        Ok(Self::Zip {
            archive: Box::new(archive),
            base: base.into(),
            by_name,
        })
    }
}

/// Copies one attachment into `dest`. `Ok(None)` means the export does not
/// contain it.
/// `Fetched` is the bytes copied and, for a folder, the file they came from.
type Fetched = std::result::Result<(u64, Option<PathBuf>), CopyLimit>;

fn fetch(store: &mut Store, reference: &str, dest: &Path, cap: u64) -> Result<Option<Fetched>> {
    let reference = reference.replace('\\', "/");
    let reference = reference.trim_start_matches("./");
    match store {
        Store::None => Ok(None),
        Store::Dir(base) => {
            let candidate = if Path::new(reference).is_absolute() {
                PathBuf::from(reference)
            } else {
                base.join(reference)
            };
            let Ok(found) = candidate.canonicalize() else {
                return Ok(None);
            };
            if !found.starts_with(&*base) || !found.is_file() {
                return Ok(None);
            }
            let file = fs::File::open(&found).with_context(|| format!("open {reference}"))?;
            let out =
                fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
            let copied = copy_capped(file, std::io::BufWriter::new(out), cap, None, 1, u64::MAX)?;
            Ok(Some(copied.map(|n| (n, Some(found)))))
        }
        Store::Zip {
            archive,
            base,
            by_name,
        } => {
            let direct = format!("{base}{reference}");
            let name = if archive.index_for_name(&direct).is_some() {
                direct
            } else {
                match by_name.get(file_name(reference)) {
                    Some(Some(name)) => name.clone(),
                    _ => return Ok(None),
                }
            };
            let entry = archive
                .by_name(&name)
                .with_context(|| format!("open {name}"))?;
            if entry.is_dir() || entry.is_symlink() || entry.encrypted() {
                return Ok(None);
            }
            let compressed = Some(entry.compressed_size());
            let out =
                fs::File::create(dest).with_context(|| format!("create {}", dest.display()))?;
            let copied = copy_capped(
                entry,
                std::io::BufWriter::new(out),
                cap,
                compressed,
                MAX_COMPRESSION_RATIO,
                RATIO_FLOOR,
            )?;
            Ok(Some(copied.map(|n| (n, None))))
        }
    }
}

/// Lays conversations out as text units and pulls attachments in after the
/// message that sent them.
struct Writer<'a> {
    ctx: &'a JobContext,
    members: &'a dyn MemberImporter,
    source: &'a SourceRecord,
    store: Store,
    seen: &'a ImportedAttachments,
    attachments: bool,
    dir: Option<PathBuf>,
    imported: usize,
    missing: usize,
    over_limit: usize,
    units: Vec<Unit>,
    warnings: Vec<String>,
    // The text unit being written.
    text: String,
    day: Option<String>,
    first: Option<String>,
    last: Option<String>,
}

impl Writer<'_> {
    fn warn(&mut self, message: String) {
        let name = anytopdf_core::basename(&self.source.path);
        self.warnings.push(
            Diagnostic::new(
                DiagnosticCode::MembersNotImported,
                format!("{name}: {message}"),
            )
            .to_string(),
        );
    }

    fn flush(&mut self, conv: &Conversation) {
        if self.text.is_empty() {
            return;
        }
        let mut unit = Unit::text(self.source.id, std::mem::take(&mut self.text));
        unit.metadata
            .insert("chat.platform".into(), conv.platform.into());
        unit.metadata
            .insert("chat.title".into(), conv.title.clone());
        if let Some(first) = self.first.take() {
            unit.metadata.insert("chat.start".into(), first);
        }
        if let Some(last) = self.last.take() {
            unit.metadata.insert("chat.end".into(), last);
        }
        self.units.push(unit);
    }

    /// Opens a continuation unit; returns whether one was opened.
    fn start_unit(&mut self, conv: &Conversation) -> bool {
        if !self.text.is_empty() {
            return false;
        }
        self.text = format!("{} chat: {} (continued)\n", conv.platform, conv.title);
        true
    }

    fn conversation(&mut self, conv: &Conversation) {
        self.day = None;
        let participants = conv.participants();
        self.text = format!("{} chat: {}\n", conv.platform, conv.title);
        if !participants.is_empty() {
            self.text
                .push_str(&format!("Participants: {}\n", participants.join(", ")));
        }
        let dates: Vec<&str> = conv
            .messages
            .iter()
            .filter_map(|m| m.date.as_deref())
            .collect();
        let span = match (dates.first(), dates.last()) {
            (Some(a), Some(b)) if a != b => format!(" ({a} to {b})"),
            (Some(a), _) => format!(" ({a})"),
            _ => String::new(),
        };
        self.text
            .push_str(&format!("Messages: {}{span}\n", conv.messages.len()));
        for message in &conv.messages {
            self.message(conv, message);
        }
        self.flush(conv);
    }

    fn message(&mut self, conv: &Conversation, m: &Message) {
        let fresh = self.start_unit(conv);
        let new_day = m.date.is_some() && m.date != self.day;
        if new_day {
            self.day = m.date.clone();
        }
        if (new_day || fresh)
            && let Some(day) = &self.day
        {
            self.text.push_str(&format!("\n-- {day} --\n"));
        }
        let mut line = String::new();
        if let Some(time) = &m.time {
            line.push_str(time);
            line.push(' ');
        }
        if let Some(sender) = &m.sender {
            line.push_str(sender);
            line.push_str(": ");
        }
        let mut body: Vec<String> = m.text.lines().map(str::to_string).collect();
        let notes: Vec<String> = m
            .attachments
            .iter()
            .map(|a| {
                format!(
                    "[{}: {}]",
                    a.kind.as_deref().unwrap_or("attachment"),
                    a.display_name()
                )
            })
            .collect();
        match body.first_mut() {
            Some(first) if !notes.is_empty() => {
                first.push(' ');
                first.push_str(&notes.join(" "));
            }
            None => body.push(notes.join(" ")),
            _ => {}
        }
        line.push_str(&body[0]);
        for more in &body[1..] {
            line.push_str("\n    ");
            line.push_str(more);
        }
        self.text.push_str(line.trim_end());
        self.text.push('\n');
        let stamp = match (&m.date, &m.time) {
            (Some(d), Some(t)) => Some(format!("{d} {t}")),
            (Some(d), None) => Some(d.clone()),
            _ => None,
        };
        if stamp.is_some() {
            if self.first.is_none() {
                self.first = stamp.clone();
            }
            self.last = stamp.clone();
        }
        let mut attached = Vec::new();
        if self.attachments {
            for attachment in &m.attachments {
                attached.extend(self.attachment(attachment, m, stamp.as_deref()));
            }
        }
        if !attached.is_empty() {
            self.flush(conv);
            self.units.extend(attached);
        } else if self.text.len() >= UNIT_TEXT_BYTES {
            self.flush(conv);
        }
    }

    fn attachment(&mut self, a: &AttachmentRef, m: &Message, sent: Option<&str>) -> Vec<Unit> {
        if matches!(self.store, Store::None) {
            return Vec::new();
        }
        if self.imported >= MAX_CHAT_ATTACHMENTS {
            self.over_limit += 1;
            return Vec::new();
        }
        let dir = match &self.dir {
            Some(dir) => dir.clone(),
            None => match member_dir(self.ctx, "chat") {
                Ok(dir) => {
                    self.dir = Some(dir.clone());
                    dir
                }
                Err(e) => {
                    self.warn(format!("{} was not imported: {e:#}", a.display_name()));
                    return Vec::new();
                }
            },
        };
        let dest = member_path(&dir, self.imported, a.display_name());
        let cap = MAX_ATTACHMENT_BYTES.min(self.members.remaining_bytes());
        let label = a.display_name().to_string();
        let (written, origin) = match fetch(&mut self.store, &a.reference, &dest, cap) {
            Ok(Some(Ok(fetched))) => fetched,
            Ok(None) => {
                let _ = fs::remove_file(&dest);
                self.missing += 1;
                return Vec::new();
            }
            Ok(Some(Err(limit))) => {
                let _ = fs::remove_file(&dest);
                self.warn(match limit {
                    CopyLimit::TooLarge => format!("{label} was not imported: larger than {cap} bytes"),
                    CopyLimit::Ratio => format!(
                        "{label} was not imported: compression ratio above {MAX_COMPRESSION_RATIO}:1"
                    ),
                });
                return Vec::new();
            }
            Err(e) => {
                let _ = fs::remove_file(&dest);
                self.warn(format!("{label} was not imported: {e:#}"));
                return Vec::new();
            }
        };
        if let Err(e) = self.members.charge(written) {
            let _ = fs::remove_file(&dest);
            self.warn(format!("{label} was not imported: {e:#}"));
            return Vec::new();
        }
        self.imported += 1;
        if let Some(origin) = origin {
            self.seen.record(&self.ctx.workspace, origin);
        }
        let (mut units, warnings) = import_members(
            self.ctx,
            self.members,
            self.source,
            vec![MemberFile { label, path: dest }],
        );
        self.warnings.extend(warnings);
        for unit in &mut units {
            if let Some(sender) = &m.sender {
                unit.metadata.insert("chat.sender".into(), sender.clone());
            }
            if let Some(sent) = sent {
                unit.metadata.insert("chat.sent".into(), sent.into());
            }
        }
        units
    }
}

impl Plugin for ChatImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "chat".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: Vec::new(),
            mime_types: Vec::new(),
            priority: 60,
        }
    }
}

impl Importer for ChatImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        if detect(&source.path).is_some() {
            ProbeScore::MAGIC
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        self.import_with_members(ctx, source, &NoMembers)
    }

    fn import_with_members(
        &self,
        ctx: &JobContext,
        mut source: SourceRecord,
        members: &dyn MemberImporter,
    ) -> Result<ImportOutcome> {
        let detected = detect(&source.path).context("not a recognized chat export")?;
        let name = anytopdf_core::basename(&source.path);
        let mut warnings = Vec::new();
        let (conversations, store) = match &detected {
            Detected::File(format) => {
                let file = fs::File::open(&source.path)
                    .with_context(|| format!("open {}", source.path.display()))?;
                let bytes = read_capped(file, &name)?;
                let parent = source.path.parent().unwrap_or(Path::new("."));
                let (directory, container) = if *format == Format::SlackDay {
                    let folder = parent
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    (
                        Some(slack_directory_beside(&source.path, &mut warnings)),
                        folder,
                    )
                } else {
                    (None, name.clone())
                };
                let conversations = parse_format(
                    *format,
                    bytes,
                    &name,
                    &container,
                    &self.opts,
                    directory.as_ref(),
                    &mut warnings,
                )?;
                let store = match parent.canonicalize() {
                    Ok(base) if *format != Format::SlackDay => Store::Dir(base),
                    _ => Store::None,
                };
                (conversations, store)
            }
            Detected::Zip { entry, format } => {
                let mut archive = open_zip(&source.path).context("read zip archive")?;
                let bytes = read_entry(&mut archive, entry)?;
                let conversations = parse_format(
                    *format,
                    bytes,
                    file_name(entry),
                    &name,
                    &self.opts,
                    None,
                    &mut warnings,
                )?;
                (
                    conversations,
                    Store::zip(&source.path, parent_prefix(entry))?,
                )
            }
            Detected::SlackZip { root } => (
                parse_slack_zip(&source.path, root, &mut warnings)?,
                Store::None,
            ),
        };

        let platform = conversations[0].platform;
        let total: usize = conversations.iter().map(|c| c.messages.len()).sum();
        source
            .metadata
            .insert("chat.platform".into(), platform.into());
        source
            .metadata
            .insert("chat.messages".into(), total.to_string());
        if let [only] = conversations.as_slice() {
            source
                .metadata
                .insert("chat.title".into(), only.title.clone());
        } else {
            source
                .metadata
                .insert("chat.conversations".into(), conversations.len().to_string());
        }

        let mut writer = Writer {
            ctx,
            members,
            source: &source,
            store,
            seen: &self.imported,
            attachments: self.opts.attachments,
            dir: None,
            imported: 0,
            missing: 0,
            over_limit: 0,
            units: Vec::new(),
            warnings,
            text: String::new(),
            day: None,
            first: None,
            last: None,
        };
        for conv in &conversations {
            writer.conversation(conv);
        }
        if writer.missing > 0 {
            let missing = writer.missing;
            writer.warn(format!(
                "{missing} attachments named in the chat are not in the export"
            ));
        }
        if writer.over_limit > 0 {
            let over = writer.over_limit;
            writer.warn(format!(
                "{over} attachments beyond the first {MAX_CHAT_ATTACHMENTS} were not imported"
            ));
        }
        let Writer {
            units, warnings, ..
        } = writer;
        Ok(ImportOutcome {
            source,
            units,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinOptions, DiscoveryOptions, discover_inputs, register_builtins};
    use std::io::{Cursor, Write};
    use zip::write::SimpleFileOptions;

    fn registry(chat: ChatOptions) -> Registry {
        let mut registry = Registry::default();
        register_builtins(
            &mut registry,
            BuiltinOptions {
                ocr: crate::OcrOptions {
                    mode: crate::OcrMode::Off,
                    ..Default::default()
                },
                chat,
                ..Default::default()
            },
        );
        registry
    }

    fn png() -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::RgbImage::new(4, 3)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            for (name, data) in entries {
                zip.start_file(*name, SimpleFileOptions::default()).unwrap();
                zip.write_all(data).unwrap();
            }
            zip.finish().unwrap();
        }
        out.into_inner()
    }

    fn texts(graph: &DocumentGraph) -> Vec<String> {
        graph
            .units
            .iter()
            .map(|u| match u.kind {
                UnitKind::Text => u.visible_text.clone().unwrap_or_default(),
                kind => format!("<{kind:?}>"),
            })
            .collect()
    }

    /// Warnings other than optional providers missing from the test machine.
    fn problems(run: &PipelineRun) -> Vec<&Diagnostic> {
        run.warnings
            .iter()
            .filter(|w| w.code != DiagnosticCode::ProviderMissing)
            .collect()
    }

    fn convert(paths: &[PathBuf], chat: ChatOptions) -> PipelineRun {
        Pipeline::new(registry(chat)).ingest(paths, true).unwrap()
    }

    const WHATSAPP: &str = "[06/10/2026, 09:44:10] Adeel: Beach today?\n\
[06/10/2026, 09:45:00] Sara: \u{200e}<attached: 00000001-PHOTO.png>\n\
[07/10/2026, 08:00:00] Adeel: Lovely\nsee you\n";

    #[test]
    fn whatsapp_zip_renders_conversation_with_photo_inline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("WhatsApp Chat - Family.zip");
        let photo = png();
        fs::write(
            &path,
            zip(&[
                ("_chat.txt", WHATSAPP.as_bytes()),
                ("00000001-PHOTO.png", &photo),
            ]),
        )
        .unwrap();
        let source = SourceRecord::new(path.clone());
        assert_eq!(
            registry(ChatOptions::default())
                .importer_for(&source)
                .unwrap()
                .descriptor()
                .name,
            "chat"
        );
        let run = convert(&[path], ChatOptions::default());
        assert!(problems(&run).is_empty(), "{:?}", run.warnings);
        let source = &run.graph.sources[0];
        assert_eq!(source.metadata["chat.platform"], "WhatsApp");
        assert_eq!(source.metadata["chat.title"], "Family");
        assert_eq!(source.metadata["chat.messages"], "3");
        let texts = texts(&run.graph);
        assert_eq!(texts.len(), 3, "{texts:#?}");
        assert_eq!(
            texts[0],
            "WhatsApp chat: Family\nParticipants: Adeel, Sara\n\
Messages: 3 (2026-10-06 to 2026-10-07)\n\n-- 2026-10-06 --\n\
09:44:10 Adeel: Beach today?\n09:45:00 Sara: [attachment: 00000001-PHOTO.png]\n"
        );
        assert_eq!(texts[1], "<Visual>");
        assert_eq!(
            texts[2],
            "WhatsApp chat: Family (continued)\n\n-- 2026-10-07 --\n\
08:00:00 Adeel: Lovely\n    see you\n"
        );
        let photo = &run.graph.units[1];
        assert_eq!(photo.source_id, source.id);
        assert_eq!(photo.metadata["chat.sender"], "Sara");
        assert_eq!(photo.metadata["chat.sent"], "2026-10-06 09:45:00");
        assert_eq!(photo.metadata["container.member"], "00000001-PHOTO.png");
        let first = &run.graph.units[0];
        assert_eq!(first.metadata["chat.start"], "2026-10-06 09:44:10");
        assert_eq!(first.metadata["chat.end"], "2026-10-06 09:45:00");
    }

    #[test]
    fn attachments_can_be_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.zip");
        let photo = png();
        fs::write(
            &path,
            zip(&[
                ("_chat.txt", WHATSAPP.as_bytes()),
                ("00000001-PHOTO.png", &photo),
            ]),
        )
        .unwrap();
        let run = convert(
            &[path],
            ChatOptions {
                attachments: false,
                ..Default::default()
            },
        );
        let texts = texts(&run.graph);
        assert_eq!(texts.len(), 1, "{texts:#?}");
        assert!(texts[0].contains("[attachment: 00000001-PHOTO.png]"));
    }

    #[test]
    fn telegram_folder_imports_each_photo_once_and_never_outside_the_folder() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("secret.txt"), "do not read").unwrap();
        let export = root.path().join("ChatExport_2026-10-06");
        fs::create_dir_all(export.join("photos")).unwrap();
        fs::write(export.join("photos/photo_1.png"), png()).unwrap();
        let json = r#"{"name": "Family", "type": "private_group", "id": 1, "messages": [
          {"id": 1, "type": "message", "date": "2026-10-06T09:44:10", "date_unixtime": "1791279850",
           "from": "Adeel", "from_id": "user1", "photo": "photos/photo_1.png", "text": "beach"},
          {"id": 2, "type": "message", "date": "2026-10-06T09:45:00", "date_unixtime": "1791279900",
           "from": "Sara", "from_id": "user2", "file": "../secret.txt", "text": "sneaky"}
        ]}"#;
        fs::write(export.join("result.json"), json).unwrap();
        let inputs = discover_inputs(&[export], &DiscoveryOptions::default()).unwrap();
        assert_eq!(inputs.len(), 2, "{inputs:?}");
        let run = convert(&inputs, ChatOptions::default());
        assert_eq!(run.graph.sources.len(), 1, "{:#?}", run.graph.sources);
        let texts = texts(&run.graph);
        assert_eq!(texts.len(), 3, "{texts:#?}");
        assert!(
            texts[0].contains("09:44:10 Adeel: beach [photo: photo_1.png]"),
            "{}",
            texts[0]
        );
        assert_eq!(texts[1], "<Visual>");
        assert!(texts[2].contains("09:45:00 Sara: sneaky [attachment: secret.txt]"));
        assert!(!texts.iter().any(|t| t.contains("do not read")));
        let warnings: Vec<String> = run.warnings.iter().map(|w| w.message.clone()).collect();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("1 attachments named in the chat are not in the export")),
            "{warnings:?}"
        );
    }

    #[test]
    fn slack_zip_becomes_one_conversation_per_channel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Acme Slack export.zip");
        let users = br#"[{"id": "U1", "name": "adeel", "real_name": "Adeel"}, {"id": "U2", "name": "sara"}]"#;
        let channels = br#"[{"id": "C1", "name": "general"}, {"id": "C2", "name": "random"}]"#;
        let dms = br#"[{"id": "D1", "members": ["U1", "U2"]}]"#;
        let day = |user: &str, text: &str, ts: &str| {
            format!(r#"[{{"type": "message", "user": "{user}", "text": "{text}", "ts": "{ts}"}}]"#)
        };
        let g1 = day("U1", "first <@U2>", "1791279850.000100");
        let g2 = day("U2", "second", "1791366250.000100");
        let r1 = day("U2", "random thought", "1791279850.000200");
        let d1 = day("U1", "private hello", "1791279850.000300");
        fs::write(
            &path,
            zip(&[
                ("users.json", users),
                ("channels.json", channels),
                ("dms.json", dms),
                ("general/2026-10-07.json", g2.as_bytes()),
                ("general/2026-10-06.json", g1.as_bytes()),
                ("random/2026-10-06.json", r1.as_bytes()),
                ("D1/2026-10-06.json", d1.as_bytes()),
            ]),
        )
        .unwrap();
        let run = convert(&[path], ChatOptions::default());
        assert!(problems(&run).is_empty(), "{:?}", run.warnings);
        let source = &run.graph.sources[0];
        assert_eq!(source.metadata["chat.platform"], "Slack");
        assert_eq!(source.metadata["chat.conversations"], "3");
        let texts = texts(&run.graph);
        assert_eq!(texts.len(), 3, "{texts:#?}");
        assert!(
            texts[0].starts_with("Slack chat: DM: Adeel, sara\n"),
            "{}",
            texts[0]
        );
        assert!(
            texts[1].starts_with("Slack chat: #general\n"),
            "{}",
            texts[1]
        );
        let first = texts[1].find("first @sara").unwrap();
        let second = texts[1].find("09:44:10 sara: second").unwrap();
        assert!(first < second, "{}", texts[1]);
        assert!(texts[1].contains("-- 2026-10-07 --"));
        assert!(texts[2].contains("random thought"));
    }

    #[test]
    fn extracted_slack_day_file_uses_the_export_user_list() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("general")).unwrap();
        fs::write(
            dir.path().join("users.json"),
            r#"[{"id": "U1", "name": "adeel", "profile": {"display_name": "Adeel A"}}]"#,
        )
        .unwrap();
        let day = dir.path().join("general/2026-10-06.json");
        fs::write(
            &day,
            r#"[{"type": "message", "user": "U1", "text": "hello", "ts": "1791279850.000100"}]"#,
        )
        .unwrap();
        let run = convert(&[day], ChatOptions::default());
        let texts = texts(&run.graph);
        assert_eq!(texts.len(), 1);
        assert!(
            texts[0].starts_with("Slack chat: #general\n"),
            "{}",
            texts[0]
        );
        assert!(texts[0].contains("09:44:10 Adeel A: hello"), "{}", texts[0]);
    }

    #[test]
    fn imessage_text_export_imports_copied_attachments() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("attachments/3")).unwrap();
        fs::write(dir.path().join("attachments/3/IMG_1.png"), png()).unwrap();
        let path = dir.path().join("+15558675309.txt");
        fs::write(
            &path,
            "May 17, 2026  5:29:42 PM\n+15558675309\nLook\nattachments/3/IMG_1.png\n\n\
May 17, 2026  5:31:00 PM\nMe\nNice\n",
        )
        .unwrap();
        let run = convert(&[path], ChatOptions::default());
        assert!(problems(&run).is_empty(), "{:?}", run.warnings);
        let texts = texts(&run.graph);
        assert_eq!(texts.len(), 3, "{texts:#?}");
        assert!(texts[0].starts_with("iMessage chat: +15558675309\n"));
        assert!(texts[0].contains("17:29:42 +15558675309: Look [attachment: IMG_1.png]"));
        assert_eq!(texts[1], "<Visual>");
        assert!(texts[2].contains("17:31:00 Me: Nice"));
    }

    #[test]
    fn long_conversations_split_into_several_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("WhatsApp Chat with Long.txt");
        let line = "x".repeat(200);
        let text: String = (0..200)
            .map(|i| format!("06/10/2026, 10:{:02} - A: {i} {line}\n", i % 60))
            .collect();
        fs::write(&path, text).unwrap();
        let run = convert(&[path], ChatOptions::default());
        let texts = texts(&run.graph);
        assert!(texts.len() >= 3, "{}", texts.len());
        assert!(texts[1].starts_with("WhatsApp chat: Long (continued)\n\n-- 2026-10-06 --\n"));
        let total: usize = texts.iter().map(|t| t.matches(&line).count()).sum();
        assert_eq!(total, 200);
    }

    #[test]
    fn plain_text_json_and_zips_are_left_to_their_importers() {
        let dir = tempfile::tempdir().unwrap();
        let registry = registry(ChatOptions::default());
        for (name, bytes) in [
            (
                "notes.txt",
                b"Meeting notes\n06/10/2026, 10:00 - A: hi\n".to_vec(),
            ),
            (
                "data.json",
                br#"{"messages": [{"role": "user", "content": "hi"}]}"#.to_vec(),
            ),
            ("files.zip", zip(&[("a.txt", b"hello")])),
        ] {
            let path = dir.path().join(name);
            fs::write(&path, bytes).unwrap();
            let source = SourceRecord::new(path);
            assert_eq!(
                ChatImporter::new(ChatOptions::default(), Default::default()).probe(&source),
                ProbeScore::NONE,
                "{name}"
            );
            assert_ne!(
                registry.importer_for(&source).unwrap().descriptor().name,
                "chat"
            );
        }
    }
}
