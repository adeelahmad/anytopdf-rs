use crate::containers::{
    CopyLimit, MemberFile, copy_capped, import_members, member_dir, member_path,
};
use anyhow::{Context, Result};
use anytopdf_core::*;
use std::fs;
use std::io::{BufReader, Read};
use std::path::Path;

/// Zip and tar (optionally gzip-compressed) archives. Members are extracted
/// into the job workspace under sanitized names and imported through the
/// registry; the archive itself contributes one index page listing them.
pub struct ArchiveImporter;

/// Largest single member extracted.
pub const MAX_ARCHIVE_MEMBER_BYTES: u64 = 512 * 1024 * 1024;
/// Largest total extracted from one archive (nested archives have their own,
/// all bounded by the pipeline's per-input budget).
pub const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
/// Entries examined per archive, including skipped ones.
pub const MAX_ARCHIVE_ENTRIES: usize = 10_000;
/// Largest uncompressed/compressed ratio a zip member may reach once it is
/// past [`RATIO_FLOOR`] bytes.
pub const MAX_COMPRESSION_RATIO: u64 = 200;
const RATIO_FLOOR: u64 = 1024 * 1024;
/// Bytes a tar stream may declare (including skipped entries) before
/// scanning stops, so a gzip bomb of skipped entries cannot spin forever.
const MAX_TAR_SCAN_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Zip,
    Tar,
    TarGz,
}

/// Zip-based document formats are not generic archives; leave them to their
/// own importers.
const ZIP_DOCUMENT_MARKERS: &[&str] = &[
    "[Content_Types].xml",
    "mimetype",
    "META-INF/MANIFEST.MF",
    "AndroidManifest.xml",
];

fn file_name_lower(path: &Path) -> String {
    anytopdf_core::basename(path).to_ascii_lowercase()
}

fn read_prefix(path: &Path, len: u64) -> Vec<u8> {
    let mut prefix = Vec::new();
    let _ = fs::File::open(path).and_then(|f| f.take(len).read_to_end(&mut prefix));
    prefix
}

fn is_zip_document(path: &Path) -> bool {
    let Ok(file) = fs::File::open(path) else {
        return false;
    };
    let Ok(archive) = zip::ZipArchive::new(BufReader::new(file)) else {
        return false;
    };
    archive
        .file_names()
        .any(|n| ZIP_DOCUMENT_MARKERS.contains(&n))
}

fn detect(path: &Path) -> Option<(Kind, ProbeScore)> {
    let name = file_name_lower(path);
    let prefix = read_prefix(path, 512);
    let zip_magic = prefix.starts_with(b"PK\x03\x04") || prefix.starts_with(b"PK\x05\x06");
    let tar_magic = prefix.len() >= 262 && &prefix[257..262] == b"ustar";
    if name.ends_with(".zip") {
        return Some((Kind::Zip, ProbeScore::MIME));
    }
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        return Some((Kind::TarGz, ProbeScore::MIME));
    }
    if name.ends_with(".tar") {
        return Some((Kind::Tar, ProbeScore::MIME));
    }
    if tar_magic {
        return Some((Kind::Tar, ProbeScore::MAGIC));
    }
    if zip_magic && !is_zip_document(path) {
        return Some((Kind::Zip, ProbeScore::MAGIC));
    }
    // A bare .gz is a single compressed file, not an archive.
    None
}

fn is_hidden(name: &str) -> bool {
    name.split(['/', '\\'])
        .any(|part| (part.starts_with('.') && part != "." && part != "..") || part == "__MACOSX")
}

struct Extracted {
    files: Vec<MemberFile>,
    listing: Vec<(String, u64)>,
    warnings: Vec<String>,
}

struct Extractor<'a> {
    archive_name: String,
    dir: std::path::PathBuf,
    members: &'a dyn MemberImporter,
    total: u64,
    out: Extracted,
}

impl Extractor<'_> {
    fn warn(&mut self, message: String) {
        self.out.warnings.push(
            Diagnostic::new(
                DiagnosticCode::MembersNotImported,
                format!("{}: {message}", self.archive_name),
            )
            .to_string(),
        );
    }

    /// Extracts one regular member. Returns false when the archive's budget
    /// is spent and extraction should stop.
    fn member(&mut self, name: &str, reader: impl Read, compressed: Option<u64>) -> Result<bool> {
        let index = self.out.listing.len();
        let path = member_path(&self.dir, index, name);
        let cap = MAX_ARCHIVE_MEMBER_BYTES
            .min(MAX_ARCHIVE_BYTES - self.total)
            .min(self.members.remaining_bytes());
        let file = fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
        let copied = copy_capped(
            reader,
            std::io::BufWriter::new(file),
            cap,
            compressed,
            MAX_COMPRESSION_RATIO,
            RATIO_FLOOR,
        );
        let written = match copied {
            Ok(Ok(written)) => written,
            Ok(Err(limit)) => {
                let _ = fs::remove_file(&path);
                let spent = cap < MAX_ARCHIVE_MEMBER_BYTES;
                self.warn(match limit {
                    CopyLimit::TooLarge if spent => {
                        format!("{name} was not imported: extraction budget exhausted")
                    }
                    CopyLimit::TooLarge => {
                        format!("{name} was not imported: larger than {cap} bytes")
                    }
                    CopyLimit::Ratio => format!(
                        "{name} was not imported: compression ratio above {MAX_COMPRESSION_RATIO}:1"
                    ),
                });
                return Ok(!(spent && limit == CopyLimit::TooLarge));
            }
            Err(e) => {
                let _ = fs::remove_file(&path);
                self.warn(format!("{name} was not imported: {e:#}"));
                return Ok(true);
            }
        };
        if let Err(e) = self.members.charge(written) {
            let _ = fs::remove_file(&path);
            self.warn(format!("{name} was not imported: {e:#}"));
            return Ok(false);
        }
        self.total += written;
        self.out.listing.push((name.to_string(), written));
        self.out.files.push(MemberFile {
            label: name.to_string(),
            path,
        });
        Ok(true)
    }
}

fn extract_zip(ex: &mut Extractor<'_>, path: &Path) -> Result<()> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut archive = zip::ZipArchive::new(BufReader::new(file)).context("read zip archive")?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        ex.warn(format!(
            "only the first {MAX_ARCHIVE_ENTRIES} of {} entries were examined",
            archive.len()
        ));
    }
    for i in 0..archive.len().min(MAX_ARCHIVE_ENTRIES) {
        let entry = match archive.by_index(i) {
            Ok(entry) => entry,
            Err(e) => {
                ex.warn(format!("entry {i} was not imported: {e}"));
                continue;
            }
        };
        let name = entry.name().to_string();
        if entry.is_dir() || is_hidden(&name) {
            continue;
        }
        if entry.is_symlink() {
            ex.warn(format!(
                "{name} was not imported: symbolic links are skipped"
            ));
            continue;
        }
        if entry.encrypted() {
            ex.warn(format!("{name} was not imported: encrypted"));
            continue;
        }
        let compressed = Some(entry.compressed_size());
        if !ex.member(&name, entry, compressed)? {
            break;
        }
    }
    Ok(())
}

fn extract_tar(ex: &mut Extractor<'_>, reader: impl Read) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    let mut scanned: u64 = 0;
    for (i, entry) in archive.entries().context("read tar archive")?.enumerate() {
        if i == MAX_ARCHIVE_ENTRIES {
            ex.warn(format!(
                "only the first {MAX_ARCHIVE_ENTRIES} entries were examined"
            ));
            break;
        }
        let entry = entry.context("read tar entry")?;
        let size = entry.header().size().unwrap_or(0);
        scanned = scanned.saturating_add(size);
        if scanned > MAX_TAR_SCAN_BYTES {
            ex.warn(format!(
                "scanning stopped after {MAX_TAR_SCAN_BYTES} declared bytes"
            ));
            break;
        }
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        let kind = entry.header().entry_type();
        if kind.is_dir() || is_hidden(&name) || kind.is_pax_global_extensions() {
            continue;
        }
        if !kind.is_file() {
            ex.warn(format!(
                "{name} was not imported: only regular files are extracted"
            ));
            continue;
        }
        if !ex.member(&name, entry, None)? {
            break;
        }
    }
    Ok(())
}

impl Plugin for ArchiveImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "archive".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: vec!["zip".into(), "tar".into(), "tgz".into(), "tar.gz".into()],
            mime_types: vec![
                "application/zip".into(),
                "application/x-tar".into(),
                "application/gzip".into(),
            ],
            priority: 50,
        }
    }
}

impl Importer for ArchiveImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        detect(&source.path).map_or(ProbeScore::NONE, |(_, score)| score)
    }

    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        self.import_with_members(ctx, source, &NoMembers)
    }

    fn import_with_members(
        &self,
        ctx: &JobContext,
        source: SourceRecord,
        members: &dyn MemberImporter,
    ) -> Result<ImportOutcome> {
        let (kind, _) =
            detect(&source.path).ok_or_else(|| anyhow::anyhow!("not a zip or tar archive"))?;
        let archive_name = anytopdf_core::basename(&source.path);
        let mut ex = Extractor {
            archive_name: archive_name.clone(),
            dir: member_dir(ctx, "archive")?,
            members,
            total: 0,
            out: Extracted {
                files: Vec::new(),
                listing: Vec::new(),
                warnings: Vec::new(),
            },
        };
        match kind {
            Kind::Zip => extract_zip(&mut ex, &source.path)?,
            Kind::Tar | Kind::TarGz => {
                let file = BufReader::new(
                    fs::File::open(&source.path)
                        .with_context(|| format!("open {}", source.path.display()))?,
                );
                if kind == Kind::TarGz {
                    extract_tar(&mut ex, flate2::read::GzDecoder::new(file))?;
                } else {
                    extract_tar(&mut ex, file)?;
                }
            }
        }
        let Extracted {
            files,
            listing,
            mut warnings,
        } = ex.out;

        let mut index = format!("Archive: {archive_name}\n");
        if listing.is_empty() {
            index.push_str("\nNo files were extracted.\n");
        } else {
            index.push('\n');
            for (name, size) in &listing {
                index.push_str(&format!("- {name} ({size} bytes)\n"));
            }
        }
        let mut units = vec![Unit::text(source.id, index)];
        let (member_units, member_warnings) = import_members(ctx, members, &source, files);
        units.extend(member_units);
        warnings.extend(member_warnings);
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
    use crate::{BuiltinOptions, register_builtins};
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    fn builtins() -> Registry {
        let mut registry = Registry::default();
        register_builtins(&mut registry, BuiltinOptions::default());
        registry
    }

    fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            for (name, data) in entries {
                if name.ends_with('/') {
                    zip.add_directory(*name, SimpleFileOptions::default())
                        .unwrap();
                } else {
                    zip.start_file(*name, SimpleFileOptions::default()).unwrap();
                    zip.write_all(data).unwrap();
                }
            }
            zip.finish().unwrap();
        }
        out.into_inner()
    }

    fn tar_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            // Write the raw name so traversal names survive the builder.
            let raw = &mut header.as_old_mut().name;
            raw[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        builder.into_inner().unwrap()
    }

    fn ingest(dir: &Path, name: &str, bytes: &[u8]) -> PipelineRun {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        Pipeline::new(builtins()).ingest(&[path], true).unwrap()
    }

    fn texts(run: &PipelineRun) -> Vec<String> {
        run.graph
            .units
            .iter()
            .map(|u| u.visible_text.clone().unwrap_or_default())
            .collect()
    }

    fn member_warnings(run: &PipelineRun) -> Vec<String> {
        run.warnings
            .iter()
            .filter(|d| d.code == DiagnosticCode::MembersNotImported)
            .map(|d| d.message.clone())
            .collect()
    }

    #[test]
    fn zip_members_are_imported_under_the_archive_source() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = zip_bytes(&[
            ("docs/", b""),
            ("docs/a.txt", b"alpha"),
            ("docs/page.html", b"<p>beta</p>"),
            (".hidden/secret.txt", b"secret"),
            ("__MACOSX/docs/._a.txt", b"junk"),
        ]);
        let run = ingest(dir.path(), "bundle.zip", &bytes);
        assert_eq!(run.graph.sources.len(), 1);
        let texts = texts(&run);
        assert_eq!(
            texts,
            [
                "Archive: bundle.zip\n\n- docs/a.txt (5 bytes)\n- docs/page.html (11 bytes)\n",
                "alpha",
                "beta\n"
            ]
        );
        assert_eq!(
            run.graph.units[2].metadata["container.member"],
            "docs/page.html"
        );
        assert!(
            run.graph
                .units
                .iter()
                .all(|u| u.source_id == run.graph.sources[0].id)
        );
        assert!(member_warnings(&run).is_empty(), "{:?}", run.warnings);
    }

    #[test]
    fn traversal_names_never_escape_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = dir.path().join("inputs");
        fs::create_dir(&inputs).unwrap();
        let evil: &[(&str, &[u8])] = &[
            ("../../escaped.txt", b"zip escape"),
            ("/abs/escaped2.txt", b"absolute"),
        ];
        let run = ingest(&inputs, "evil.zip", &zip_bytes(evil));
        assert_eq!(texts(&run).len(), 3, "{:?} {:?}", texts(&run), run.warnings);
        let run = ingest(
            &inputs,
            "evil.tar",
            &tar_bytes(&[("../escaped3.txt", b"tar escape")]),
        );
        assert_eq!(texts(&run)[1], "tar escape");
        for name in ["escaped.txt", "escaped2.txt", "escaped3.txt"] {
            assert!(!dir.path().join(name).exists(), "{name}");
            assert!(!inputs.join(name).exists(), "{name}");
        }
    }

    #[test]
    fn zip_bomb_members_are_refused_by_ratio() {
        let dir = tempfile::tempdir().unwrap();
        let zeros = vec![0u8; 8 * 1024 * 1024];
        let mut out = std::io::Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut out);
            let deflated =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("zeros.txt", deflated).unwrap();
            zip.write_all(&zeros).unwrap();
            zip.start_file("ok.txt", deflated).unwrap();
            zip.write_all(b"fine").unwrap();
            zip.finish().unwrap();
        }
        let run = ingest(dir.path(), "bomb.zip", &out.into_inner());
        let warnings = member_warnings(&run);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("zeros.txt") && warnings[0].contains("ratio"));
        assert_eq!(texts(&run)[1], "fine");
    }

    #[test]
    fn tar_gz_and_nested_archives_are_expanded() {
        let dir = tempfile::tempdir().unwrap();
        let inner = zip_bytes(&[("inner.txt", b"deep")]);
        let tar = tar_bytes(&[("notes.md", b"# Notes"), ("inner.zip", &inner)]);
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        let run = ingest(dir.path(), "bundle.tar.gz", &gz.finish().unwrap());
        let texts = texts(&run);
        assert_eq!(texts.len(), 4, "{texts:?} {:?}", run.warnings);
        assert!(texts[0].starts_with("Archive: bundle.tar.gz"));
        assert_eq!(texts[1], "# Notes");
        assert!(
            texts[2].starts_with("Archive: 0001-inner.zip"),
            "{}",
            texts[2]
        );
        assert_eq!(texts[3], "deep");
    }

    #[test]
    fn tar_links_and_unsupported_members_warn() {
        let dir = tempfile::tempdir().unwrap();
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        builder
            .append_link(&mut header, "link", "/etc/passwd")
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_size(3);
        builder
            .append_data(&mut header, "blob.bin", &[0u8, 1, 2][..])
            .unwrap();
        let run = ingest(dir.path(), "links.tar", &builder.into_inner().unwrap());
        let warnings = member_warnings(&run);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings[0].contains("link") && warnings[0].contains("regular files"));
        assert!(warnings[1].contains("blob.bin"));
        assert!(!run.warnings.iter().any(|d| d.code.is_skip()));
    }

    #[test]
    fn probe_claims_archives_but_not_zip_based_documents() {
        let dir = tempfile::tempdir().unwrap();
        let registry = builtins();
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            fs::write(&path, bytes).unwrap();
            SourceRecord::new(path)
        };
        let plain = write("download", &zip_bytes(&[("a.txt", b"a")]));
        let tar = write("backup", &tar_bytes(&[("a.txt", b"a")]));
        let docx = write(
            "report.docx",
            &zip_bytes(&[
                ("[Content_Types].xml", b"<Types/>"),
                ("word/document.xml", b"<w/>"),
            ]),
        );
        let epub = write("book", &zip_bytes(&[("mimetype", b"application/epub+zip")]));
        for source in [&plain, &tar] {
            assert_eq!(ArchiveImporter.probe(source), ProbeScore::MAGIC);
            assert_eq!(
                registry.importer_for(source).unwrap().descriptor().name,
                "archive"
            );
        }
        for source in [&docx, &epub] {
            assert_eq!(
                ArchiveImporter.probe(source),
                ProbeScore::NONE,
                "{}",
                source.path.display()
            );
        }
    }
}
