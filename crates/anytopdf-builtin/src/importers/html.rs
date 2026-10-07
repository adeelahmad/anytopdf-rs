use super::office::{file_url, page_text, rasterize, word_annotation};
use super::{TEXT_LAYER_KEY, TEXT_LAYER_NATIVE};
use crate::html::readable_text;
use crate::{chrome_path, print_to_pdf};
use anyhow::{Context, Result};
use anytopdf_core::*;
use std::fs;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

/// Local HTML files. By default the readable text becomes one text unit; with
/// `render` a headless browser prints the page and each printed page becomes an
/// image page carrying the page's own positioned text.
pub struct HtmlImporter {
    render: bool,
}

impl HtmlImporter {
    pub fn new(render: bool) -> Self {
        Self { render }
    }
}

/// How long the browser may take to print one page.
const RENDER_TIMEOUT: Duration = Duration::from_secs(120);

const SNIFF_BYTES: usize = 1024;

/// Whether a file prefix opens like an HTML document.
fn looks_like_html(prefix: &[u8]) -> bool {
    let text = String::from_utf8_lossy(prefix);
    let lower = text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .to_ascii_lowercase();
    lower.starts_with("<!doctype html") || lower.starts_with("<html")
}

impl Plugin for HtmlImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "html".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: vec!["html".into(), "htm".into(), "xhtml".into()],
            mime_types: vec!["text/html".into(), "application/xhtml+xml".into()],
            priority: 40,
        }
    }
}

impl Importer for HtmlImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        let ext = source
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if self.descriptor().extensions.iter().any(|x| x == &ext) {
            return ProbeScore::EXTENSION;
        }
        let mut prefix = Vec::new();
        let read = fs::File::open(&source.path)
            .and_then(|f| f.take(SNIFF_BYTES as u64).read_to_end(&mut prefix));
        if read.is_ok() && looks_like_html(&prefix) {
            ProbeScore::MAGIC
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, ctx: &JobContext, mut source: SourceRecord) -> Result<ImportOutcome> {
        let bytes =
            fs::read(&source.path).with_context(|| format!("read {}", source.path.display()))?;
        let mut warnings = Vec::new();
        let html = match String::from_utf8(bytes) {
            Ok(html) => html,
            Err(e) => {
                let name = anytopdf_core::basename(&source.path);
                warnings.push(
                    Diagnostic::new(
                        DiagnosticCode::LossyDecode,
                        format!("{name} is not valid UTF-8; invalid bytes were replaced"),
                    )
                    .to_string(),
                );
                String::from_utf8_lossy(e.as_bytes()).into_owned()
            }
        };
        let (page, scope) = readable_text(&html);
        if let Some(scope) = scope {
            source.metadata.insert("html.content".into(), scope.into());
        }
        let mut text = page.text;
        if let Some(title) = page.title {
            if !text.contains(&title) {
                text = format!("# {title}\n\n{text}");
            }
            source.metadata.insert("html.title".into(), title);
        }
        if self.render {
            match render_pages(ctx, &source, chrome_path()) {
                Ok(units) => {
                    return Ok(ImportOutcome {
                        units,
                        source,
                        warnings,
                    });
                }
                Err((code, why)) => {
                    let name = anytopdf_core::basename(&source.path);
                    warnings.push(
                        Diagnostic::new(
                            code,
                            format!("{name} was imported as text without page images: {why}"),
                        )
                        .to_string(),
                    );
                }
            }
        }
        Ok(ImportOutcome {
            units: vec![Unit::text(source.id, text)],
            source,
            warnings,
        })
    }
}

/// Prints the page with a headless browser and turns each printed page into an
/// image unit with the page's positioned text.
fn render_pages(
    ctx: &JobContext,
    source: &SourceRecord,
    chrome: Option<PathBuf>,
) -> std::result::Result<Vec<Unit>, (DiagnosticCode, String)> {
    let missing = |what: &str| (DiagnosticCode::ProviderMissing, format!("{what} not found"));
    let chrome = chrome.ok_or_else(|| missing("Chrome, Chromium or Edge"))?;
    let pdftoppm = which::which("pdftoppm").map_err(|_| missing("Poppler pdftoppm"))?;
    let failed = |e: anyhow::Error| (DiagnosticCode::ProviderFailed, format!("{e:#}"));
    let root = ctx.workspace.join(format!("html-{}", source.id));
    fs::create_dir_all(&root)
        .context("create the render folder")
        .map_err(failed)?;
    let page = fs::canonicalize(&source.path)
        .with_context(|| format!("resolve {}", source.path.display()))
        .map_err(failed)?;
    let pdf = root.join("page.pdf");
    print_to_pdf(&chrome, &file_url(&page), &pdf, &root, RENDER_TIMEOUT, true).map_err(failed)?;
    let images = rasterize(&pdftoppm, &pdf, &root.join("pages")).map_err(failed)?;
    let text = which::which("pdftotext")
        .ok()
        .and_then(|exe| page_text(&exe, &pdf, &root).ok())
        .unwrap_or_default();
    Ok(page_units(source, images, &text))
}

fn page_units(
    source: &SourceRecord,
    images: Vec<PathBuf>,
    text: &[Vec<super::office::TextWord>],
) -> Vec<Unit> {
    images
        .into_iter()
        .enumerate()
        .map(|(index, image)| {
            let mut unit = Unit::visual(source.id, image);
            unit.anchor = Some(Anchor::Region {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                frame: Some(index as u32),
            });
            unit.metadata
                .insert("html.page".into(), (index + 1).to_string());
            // Pages that already carry text skip OCR.
            if let Some(words) = text.get(index).filter(|w| !w.is_empty()) {
                unit.metadata
                    .insert(TEXT_LAYER_KEY.into(), TEXT_LAYER_NATIVE.into());
                unit.annotations.extend(words.iter().map(word_annotation));
            }
            unit
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinOptions, register_builtins};
    use std::path::Path;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> SourceRecord {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        SourceRecord::new(path)
    }

    fn import(dir: &Path, source: SourceRecord) -> ImportOutcome {
        let ctx = JobContext {
            workspace: dir.into(),
            quiet: true,
        };
        HtmlImporter::new(false).import(&ctx, source).unwrap()
    }

    #[test]
    fn html_page_becomes_one_text_unit_with_title() {
        let dir = tempfile::tempdir().unwrap();
        let source = write(
            dir.path(),
            "page.html",
            b"<html><head><title>Receipt 7</title></head><body><p>Paid &euro;12</p></body></html>",
        );
        let outcome = import(dir.path(), source);
        assert_eq!(outcome.units.len(), 1);
        assert_eq!(
            outcome.units[0].visible_text.as_deref(),
            Some("# Receipt 7\n\nPaid €12\n")
        );
        assert_eq!(outcome.source.metadata["html.title"], "Receipt 7");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    }

    #[test]
    fn title_repeated_in_body_is_not_duplicated() {
        let dir = tempfile::tempdir().unwrap();
        let source = write(
            dir.path(),
            "page.htm",
            b"<title>Notes</title><h1>Notes</h1><p>body</p>",
        );
        let outcome = import(dir.path(), source);
        assert_eq!(
            outcome.units[0].visible_text.as_deref(),
            Some("# Notes\n\nbody\n")
        );
    }

    #[test]
    fn non_utf8_html_is_decoded_lossily_with_coded_warning() {
        let dir = tempfile::tempdir().unwrap();
        let source = write(dir.path(), "latin1.html", b"<p>caf\xe9</p>");
        let outcome = import(dir.path(), source);
        assert_eq!(
            outcome.units[0].visible_text.as_deref(),
            Some("caf\u{FFFD}\n")
        );
        let d = Diagnostic::from_wire(&outcome.warnings[0]);
        assert_eq!(d.code, DiagnosticCode::LossyDecode);
    }

    #[test]
    fn render_without_a_browser_falls_back_to_text_with_a_warning() {
        let dir = tempfile::tempdir().unwrap();
        let source = write(dir.path(), "page.html", b"<p>Paid 12</p>");
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let err = render_pages(&ctx, &source, None).unwrap_err();
        assert_eq!(err.0, DiagnosticCode::ProviderMissing);
        assert!(err.1.contains("Chrome"), "{}", err.1);
        let outcome = HtmlImporter::new(false).import(&ctx, source).unwrap();
        assert_eq!(outcome.units[0].kind, UnitKind::Text);
    }

    #[test]
    fn render_prints_pages_with_their_text_when_a_browser_is_installed() {
        let (Some(chrome), Ok(_)) = (chrome_path(), which::which("pdftoppm")) else {
            eprintln!("skipped: needs Chrome/Chromium and pdftoppm");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let source = write(
            dir.path(),
            "invoice page.html",
            b"<html><head><title>Invoice</title></head><body><nav>Home</nav>\
              <main><h1>Invoice 42</h1><p>Total paid 128.40</p>\
              <img src=\"https://example.invalid/pixel.png\"></main></body></html>",
        );
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let units = render_pages(&ctx, &source, Some(chrome)).unwrap();
        assert!(!units.is_empty());
        let first = &units[0];
        assert_eq!(first.kind, UnitKind::Visual);
        assert!(first.visual_path.as_ref().unwrap().starts_with(dir.path()));
        assert_eq!(first.metadata["html.page"], "1");
        if which::which("pdftotext").is_ok() {
            let text: Vec<&str> = first.annotations.iter().map(|a| a.text.as_str()).collect();
            let text = text.join(" ");
            assert!(
                text.contains("Invoice 42") && text.contains("128.40"),
                "{text}"
            );
            assert_eq!(first.metadata[TEXT_LAYER_KEY], TEXT_LAYER_NATIVE);
        }
    }

    #[test]
    fn html_outranks_text_sniffing_by_extension_and_by_doctype() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::default();
        register_builtins(&mut registry, BuiltinOptions::default());
        let by_ext = write(dir.path(), "page.HTM", b"<p>x</p>");
        let by_magic = write(
            dir.path(),
            "saved",
            b"\xef\xbb\xbf  <!DOCTYPE html><p>x</p>",
        );
        let plain = write(dir.path(), "notes", b"<p> is a tag in prose\n");
        for source in [&by_ext, &by_magic] {
            assert_eq!(
                registry.importer_for(source).unwrap().descriptor().name,
                "html",
                "{}",
                source.path.display()
            );
        }
        assert_eq!(
            registry.importer_for(&plain).unwrap().descriptor().name,
            "text"
        );
    }
}
