use crate::html::readable_text;
use anyhow::{Context, Result};
use anytopdf_core::*;
use std::fs;
use std::io::Read;

pub struct HtmlImporter;

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

    fn import(&self, _ctx: &JobContext, mut source: SourceRecord) -> Result<ImportOutcome> {
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
        Ok(ImportOutcome {
            units: vec![Unit::text(source.id, text)],
            source,
            warnings,
        })
    }
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
        HtmlImporter.import(&ctx, source).unwrap()
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
