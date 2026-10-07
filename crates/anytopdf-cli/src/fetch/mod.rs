//! URL inputs. Each http(s) argument is fetched into a temporary directory before
//! discovery and replaced by the downloaded files, so the normal importers handle
//! them; every resulting source records where it came from as `url.*` metadata.

mod guard;
mod http;
mod links;
mod media;

use crate::exit::{CliError, ExitClass, fail, tag};
use anytopdf_builtin::{chrome_path, print_to_pdf, ytdlp_path};
use anytopdf_core::{Annotation, AnnotationKind, Diagnostic, DiagnosticCode, DocumentGraph, Unit};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tempfile::TempDir;
use ureq::http::Uri;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum UrlMode {
    /// yt-dlp for known video and podcast hosts, a plain download otherwise.
    Auto,
    /// Always download the URL itself (web pages become HTML inputs).
    Page,
    /// Always hand the URL to yt-dlp.
    Media,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum SnapshotMode {
    /// Snapshot when a Chrome-family browser and Poppler pdftoppm are installed.
    Auto,
    /// Always try to snapshot; warn when no browser is found.
    On,
    /// Never snapshot.
    Off,
}

#[derive(Debug, Clone)]
pub(crate) struct UrlOptions {
    pub(crate) mode: UrlMode,
    pub(crate) snapshot: SnapshotMode,
    pub(crate) sub_langs: String,
    pub(crate) max_height: u32,
    pub(crate) max_mb: u64,
    pub(crate) timeout: Duration,
    pub(crate) allow_private: bool,
}

/// One fetched file and the `url.*` metadata its source should carry.
pub(crate) struct Origin {
    pub(crate) path: PathBuf,
    pub(crate) metadata: Vec<(String, String)>,
    /// Video chapters, added as scene annotations on the units they cover.
    pub(crate) chapters: Vec<media::Chapter>,
}

pub(crate) struct Fetched {
    /// The command-line inputs with each URL replaced by its downloaded files.
    pub(crate) inputs: Vec<PathBuf>,
    /// One entry per command-line input, for naming the default output.
    pub(crate) names: Vec<PathBuf>,
    pub(crate) origins: Vec<Origin>,
    pub(crate) warnings: Vec<Diagnostic>,
    /// Holds the downloads until the conversion is finished.
    pub(crate) dir: Option<TempDir>,
}

pub(crate) fn is_url(input: &Path) -> bool {
    input.to_str().is_some_and(|s| {
        let lower = s.get(..8).unwrap_or(s).to_ascii_lowercase();
        lower.starts_with("https://") || lower.starts_with("http://")
    })
}

pub(crate) fn fetch_inputs(
    inputs: &[PathBuf],
    lists: &[PathBuf],
    opts: &UrlOptions,
    progress: bool,
) -> Result<Fetched, CliError> {
    let mut fetched = Fetched {
        inputs: Vec::new(),
        names: Vec::new(),
        origins: Vec::new(),
        warnings: Vec::new(),
        dir: None,
    };
    if lists.is_empty() && !inputs.iter().any(|p| is_url(p)) {
        fetched.inputs = inputs.to_vec();
        fetched.names = inputs.to_vec();
        return Ok(fetched);
    }
    let dir = tempfile::Builder::new()
        .prefix("anytopdf-url-")
        .tempdir()
        .map_err(CliError::from)?;
    let agent = http::agent(opts.timeout);
    let mut next = 0usize;
    let mut subdir = || -> Result<PathBuf, CliError> {
        let sub = dir.path().join(next.to_string());
        next += 1;
        std::fs::create_dir_all(&sub).map_err(CliError::from)?;
        Ok(sub)
    };
    for input in inputs {
        let Some(url) = input.to_str().filter(|_| is_url(input)) else {
            fetched.inputs.push(input.clone());
            fetched.names.push(input.clone());
            continue;
        };
        if progress {
            eprintln!("fetch: {url}");
        }
        let (name, origins) = fetch_one(&agent, url, &subdir()?, opts, &mut fetched.warnings)?;
        // `default_output` takes the file stem; the suffix keeps dots in the name.
        fetched.names.push(PathBuf::from(format!("{name}.url")));
        fetched.add(origins);
    }
    for list in lists {
        let links = tag(ExitClass::Input, links::read_links(list))?;
        let list_name = anytopdf_core::basename(list);
        let stem = list
            .file_stem()
            .map_or_else(|| "links".into(), |s| s.to_string_lossy().into_owned());
        fetched.names.push(PathBuf::from(format!("{stem}.list")));
        for (i, link) in links.iter().enumerate() {
            if progress {
                eprintln!("fetch [{}/{}]: {}", i + 1, links.len(), link.url);
            }
            // One unreachable link skips that link, not the whole list.
            match fetch_one(&agent, &link.url, &subdir()?, opts, &mut fetched.warnings) {
                Ok((_, mut origins)) => {
                    for origin in &mut origins {
                        origin.label(link, &list_name);
                    }
                    fetched.add(origins);
                }
                Err(e) => fetched.warnings.push(Diagnostic::for_input(
                    DiagnosticCode::InputUnreadable,
                    &link.url,
                    format!("{}: {:#}", link.url, e.error),
                )),
            }
        }
    }
    fetched.dir = Some(dir);
    Ok(fetched)
}

impl Fetched {
    /// The URL-only providers this run used: yt-dlp for media, Chrome for snapshots.
    pub(crate) fn used_providers(&self) -> Vec<&'static str> {
        let kind = |k: &str| {
            self.origins.iter().any(|o| {
                o.metadata
                    .iter()
                    .any(|(key, v)| key == "url.kind" && v == k)
            })
        };
        let mut used = Vec::new();
        if kind("media") {
            used.push("yt-dlp");
        }
        if kind("snapshot") {
            used.push("chrome");
        }
        used
    }

    fn add(&mut self, origins: Vec<Origin>) {
        for origin in origins {
            self.inputs.push(origin.path.clone());
            self.origins.push(origin);
        }
    }
}

impl Origin {
    /// Bookmark title and folders for the PDF outline, plus the list it came from.
    fn label(&mut self, link: &links::Link, list: &str) {
        let snapshot = self
            .metadata
            .iter()
            .any(|(k, v)| k == "url.kind" && v == "snapshot");
        if let Some(title) = &link.title {
            let title = if snapshot {
                format!("{title} (snapshot)")
            } else {
                title.clone()
            };
            self.metadata.push(("outline.title".into(), title));
        }
        if !link.folders.is_empty() {
            self.metadata
                .push(("outline.folders".into(), link.folders.join("\n")));
        }
        self.metadata.push(("url.list".into(), list.to_string()));
    }
}

fn fetch_one(
    agent: &ureq::Agent,
    url: &str,
    dir: &Path,
    opts: &UrlOptions,
    warnings: &mut Vec<Diagnostic>,
) -> Result<(String, Vec<Origin>), CliError> {
    let uri: Uri = tag(
        ExitClass::Usage,
        url.parse::<Uri>()
            .map_err(|e| anyhow::anyhow!("invalid URL {url}: {e}")),
    )?;
    tag(ExitClass::Input, guard::check(&uri, opts.allow_private))?;
    let host = uri.host().unwrap_or_default();
    let use_media = match opts.mode {
        UrlMode::Media => true,
        UrlMode::Page => false,
        UrlMode::Auto => media::is_media_host(host),
    };
    let source = |kind: &str| {
        vec![
            ("url.source".to_string(), url.to_string()),
            ("url.kind".to_string(), kind.to_string()),
        ]
    };
    if use_media {
        let Some(exe) = ytdlp_path() else {
            return Err(fail(
                ExitClass::Provider,
                format!(
                    "{url} needs yt-dlp; install it (pip install yt-dlp, brew install yt-dlp \
                     or winget install yt-dlp.yt-dlp) or pass --url-mode page"
                ),
            ));
        };
        let got = tag(ExitClass::Input, media::download(&exe, url, dir, opts))?;
        let name = got
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| slug(&uri));
        let mut metadata = source("media");
        metadata.extend(got.metadata);
        return Ok((
            name,
            vec![Origin {
                path: got.path,
                metadata,
                chapters: got.chapters,
            }],
        ));
    }
    let name = slug(&uri);
    let max_bytes = opts.max_mb.saturating_mul(1024 * 1024);
    let got = tag(
        ExitClass::Input,
        http::download(agent, url, dir, &name, max_bytes, opts.allow_private),
    )?;
    let mut origins = Vec::new();
    if got.is_html()
        && let Some(chrome) = snapshot_browser(opts.snapshot, url, warnings)
    {
        let out = dir.join(format!("{name}.snapshot.pdf"));
        match print_to_pdf(&chrome, &got.final_url, &out, dir, opts.timeout, false) {
            Ok(()) => origins.push(Origin {
                path: canonical(out),
                metadata: source("snapshot"),
                chapters: Vec::new(),
            }),
            Err(e) => warnings.push(Diagnostic::new(
                DiagnosticCode::ProviderFailed,
                format!("page snapshot of {url} failed: {e:#}"),
            )),
        }
    }
    let mut metadata = source(if got.is_html() { "page" } else { "file" });
    if got.final_url != url {
        metadata.push(("url.final".into(), got.final_url.clone()));
    }
    if !got.content_type.is_empty() {
        metadata.push(("url.content-type".into(), got.content_type.clone()));
    }
    origins.insert(
        0,
        Origin {
            path: canonical(got.path),
            metadata,
            chapters: Vec::new(),
        },
    );
    Ok((name, origins))
}

fn canonical(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn snapshot_browser(
    mode: SnapshotMode,
    url: &str,
    warnings: &mut Vec<Diagnostic>,
) -> Option<PathBuf> {
    match mode {
        SnapshotMode::Off => None,
        SnapshotMode::Auto => chrome_path().filter(|_| which::which("pdftoppm").is_ok()),
        SnapshotMode::On => {
            let found = chrome_path();
            if found.is_none() {
                warnings.push(Diagnostic::new(
                    DiagnosticCode::ProviderMissing,
                    format!(
                        "no Chrome, Chromium or Edge found to snapshot {url}; \
                         install one or set ANYTOPDF_CHROME"
                    ),
                ));
            }
            found
        }
    }
}

/// A file-name-safe name for a URL: host and path, e.g. `example.com-blog-post`.
pub(crate) fn slug(uri: &Uri) -> String {
    let host = uri.host().unwrap_or("page");
    let host = host.strip_prefix("www.").unwrap_or(host);
    let path = uri.path().trim_matches('/');
    let path = match path.rsplit_once('/') {
        Some((dir, last)) => format!("{dir}/{}", strip_ext(last)),
        None => strip_ext(path).to_string(),
    };
    let raw = if path.is_empty() {
        host.to_string()
    } else {
        format!("{host}-{path}")
    };
    let mut out = String::new();
    for c in raw.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            c
        } else {
            '-'
        };
        if !(c == '-' && out.ends_with('-')) {
            out.push(c);
        }
    }
    let out: String = out.trim_matches(['-', '.']).chars().take(80).collect();
    let out = out.trim_end_matches(['-', '.']).to_string();
    if out.is_empty() { "page".into() } else { out }
}

fn strip_ext(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && ext.len() <= 5 => stem,
        _ => name,
    }
}

/// Record each fetched source's URL and fetch time.
pub(crate) fn annotate(graph: &mut DocumentGraph, origins: &[Origin], fetched_at: &str) {
    if origins.is_empty() {
        return;
    }
    for source in &mut graph.sources {
        let path = source
            .path
            .canonicalize()
            .unwrap_or_else(|_| source.path.clone());
        if let Some(origin) = origins.iter().find(|o| o.path == path) {
            source.metadata.extend(origin.metadata.iter().cloned());
            source
                .metadata
                .insert("url.fetched".into(), fetched_at.to_string());
            let id = source.id;
            add_chapters(graph_units(&mut graph.units, id), &origin.chapters);
        }
    }
}

fn graph_units(units: &mut [Unit], source: Uuid) -> impl Iterator<Item = &mut Unit> {
    units.iter_mut().filter(move |u| u.source_id == source)
}

/// Tag each timed unit with the chapter it falls in.
fn add_chapters<'a>(units: impl Iterator<Item = &'a mut Unit>, chapters: &[media::Chapter]) {
    if chapters.is_empty() {
        return;
    }
    for unit in units {
        let Some(at) = unit.time_range.as_ref().map(|t| t.start_seconds) else {
            continue;
        };
        let chapter = chapters
            .iter()
            .find(|c| c.range.start_seconds <= at && at < c.range.end_seconds);
        if let Some(chapter) = chapter {
            let mut note = Annotation::text(
                AnnotationKind::Scene,
                "yt-dlp",
                format!("chapter: {}", chapter.title),
            );
            note.time_range = Some(chapter.range);
            unit.annotations.push(note);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_and_https_arguments_are_urls() {
        assert!(is_url(Path::new("https://example.com/")));
        assert!(is_url(Path::new("HTTP://example.com")));
        assert!(!is_url(Path::new("notes.txt")));
        assert!(!is_url(Path::new("./https:/x")));
        assert!(!is_url(Path::new("ftp://example.com/x")));
    }

    #[test]
    fn slugs_are_short_safe_file_names() {
        let s = |u: &str| slug(&u.parse::<Uri>().unwrap());
        assert_eq!(s("https://www.example.com/"), "example.com");
        assert_eq!(
            s("https://example.com/blog/my-post.html?x=1"),
            "example.com-blog-my-post"
        );
        assert_eq!(s("http://example.com/a%20b/c"), "example.com-a-20b-c");
        assert!(s(&format!("https://example.com/{}", "x".repeat(200))).len() <= 80);
    }

    #[test]
    fn plain_inputs_pass_through_without_a_download_directory() {
        let opts = UrlOptions {
            mode: UrlMode::Auto,
            snapshot: SnapshotMode::Off,
            sub_langs: "en".into(),
            max_height: 720,
            max_mb: 1,
            timeout: Duration::from_secs(1),
            allow_private: false,
        };
        let inputs = vec![PathBuf::from("a.txt")];
        let fetched = fetch_inputs(&inputs, &[], &opts, false)
            .map_err(|e| e.error)
            .unwrap();
        assert_eq!(fetched.inputs, inputs);
        assert!(fetched.dir.is_none());
    }

    #[test]
    fn chapters_tag_the_units_they_cover() {
        use anytopdf_core::TimeRange;
        let source = Uuid::new_v4();
        let mut units: Vec<Unit> = [0.0, 40.0, 95.0]
            .into_iter()
            .map(|t| {
                let mut u = Unit::text(source, String::new());
                u.time_range = Some(TimeRange {
                    start_seconds: t,
                    end_seconds: t + 1.0,
                });
                u
            })
            .collect();
        units.push(Unit::text(source, "untimed".into()));
        let chapters =
            [("Intro", 0.0, 30.0), ("Demo", 30.0, 90.0)].map(|(t, s, e)| media::Chapter {
                range: TimeRange {
                    start_seconds: s,
                    end_seconds: e,
                },
                title: t.into(),
            });
        add_chapters(units.iter_mut(), &chapters);
        let texts: Vec<Vec<&str>> = units
            .iter()
            .map(|u| u.annotations.iter().map(|a| a.text.as_str()).collect())
            .collect();
        assert_eq!(
            texts,
            [
                vec!["chapter: Intro"],
                vec!["chapter: Demo"],
                vec![],
                vec![]
            ]
        );
    }
}
