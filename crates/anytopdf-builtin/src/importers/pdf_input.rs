use super::office::{TextLine, line_annotation, page_text, rasterize};
use super::{TEXT_LAYER_KEY, TEXT_LAYER_NATIVE};
use anyhow::{Context, Result};
use anytopdf_core::*;
use std::collections::BTreeMap;
use std::path::Path;

/// Existing PDFs. With Poppler's `pdftoppm` every page is rendered to an
/// image page, and the PDF's own text (line boxes from `pdftotext
/// -bbox-layout`, else the built-in extractor's page text) is attached the
/// same way the Office importer does it, so OCR only runs on pages without
/// text. Without `pdftoppm` the page text is imported as text pages.
pub struct PdfInputImporter;

/// Decompressed content-stream bytes read per page by the built-in text
/// extractor, so a small PDF cannot inflate without bound.
const MAX_PAGE_CONTENT_BYTES: usize = 64 * 1024 * 1024;
const BUILTIN_TEXT_PROVIDER: &str = "pdf-text";

impl Plugin for PdfInputImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "pdf-input".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: vec!["pdf".into()],
            mime_types: vec!["application/pdf".into()],
            priority: 50,
        }
    }
}

/// Plain text per page (1-based page number to text) from the built-in parser.
fn builtin_page_text(pdf: &Path) -> Result<BTreeMap<u32, String>> {
    let doc = lopdf::Document::load(pdf).context("parse PDF")?;
    anyhow::ensure!(!doc.is_encrypted(), "PDF is encrypted");
    let mut pages = BTreeMap::new();
    for number in doc.get_pages().into_keys() {
        let text = doc
            .extract_text_with_limit(&[number], MAX_PAGE_CONTENT_BYTES)
            .unwrap_or_default();
        pages.insert(number, text.trim().to_string());
    }
    Ok(pages)
}

fn native_text(text: String) -> Annotation {
    let mut a = Annotation::text(AnnotationKind::Ocr, BUILTIN_TEXT_PROVIDER, text);
    a.confidence = Some(1.0);
    a.attributes
        .insert("text_source".into(), TEXT_LAYER_NATIVE.into());
    a
}

impl Importer for PdfInputImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        if source.detected_type.as_deref() == Some("application/pdf") {
            return ProbeScore::MAGIC;
        }
        let ext = source
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext == "pdf" {
            ProbeScore::EXTENSION
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        let name = anytopdf_core::basename(&source.path);
        let mut warnings = Vec::new();
        let Ok(pdftoppm) = which::which("pdftoppm") else {
            let units: Vec<Unit> = builtin_page_text(&source.path)?
                .into_iter()
                .filter(|(_, text)| !text.is_empty())
                .map(|(number, text)| {
                    let mut unit = Unit::text(source.id, text);
                    unit.metadata.insert("pdf.page".into(), number.to_string());
                    unit
                })
                .collect();
            anyhow::ensure!(
                !units.is_empty(),
                "PDF has no extractable text; install Poppler (pdftoppm) to render and OCR its pages"
            );
            warnings.push(
                Diagnostic::new(
                    DiagnosticCode::ProviderMissing,
                    format!("pdftoppm unavailable; {name} was imported as text without page images"),
                )
                .to_string(),
            );
            return Ok(ImportOutcome {
                source,
                units,
                warnings,
            });
        };

        let root = ctx.workspace.join(format!("pdf-{}", source.id));
        let images = rasterize(&pdftoppm, &source.path, &root.join("pages"))?;

        // Positioned lines from pdftotext; otherwise unpositioned page text.
        let lines: Option<Vec<Vec<TextLine>>> = which::which("pdftotext")
            .ok()
            .and_then(|exe| page_text(&exe, &source.path, &root).ok());
        let plain = if lines.is_none() {
            builtin_page_text(&source.path).ok()
        } else {
            None
        };

        let mut units = Vec::new();
        for (index, image) in images.into_iter().enumerate() {
            let number = index as u32 + 1;
            let mut unit = Unit::visual(source.id, image);
            unit.anchor = Some(Anchor::Region {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                frame: Some(index as u32),
            });
            unit.metadata.insert("pdf.page".into(), number.to_string());
            if let Some(page) = lines.as_ref().and_then(|l| l.get(index)) {
                unit.annotations.extend(page.iter().map(line_annotation));
            } else if let Some(text) = plain
                .as_ref()
                .and_then(|p| p.get(&number))
                .filter(|t| !t.is_empty())
            {
                unit.annotations.push(native_text(text.clone()));
            }
            if !unit.annotations.is_empty() {
                unit.metadata
                    .insert(TEXT_LAYER_KEY.into(), TEXT_LAYER_NATIVE.into());
            }
            units.push(unit);
        }
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
    use std::fs;
    use std::path::PathBuf;

    /// A one-page PDF whose text layer says `Hello anytopdf`.
    fn sample_pdf(dir: &Path) -> PathBuf {
        use lopdf::content::{Content, Operation};
        use lopdf::{Document, Object, Stream, dictionary};
        let mut doc = Document::with_version("1.5");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier",
        });
        let resources_id = doc.add_object(dictionary! {
            "Font" => dictionary! { "F1" => font_id },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 36.into()]),
                Operation::new("Td", vec![72.into(), 700.into()]),
                Operation::new("Tj", vec![Object::string_literal("Hello anytopdf")]),
                Operation::new("ET", vec![]),
            ],
        };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        });
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![page_id.into()],
                "Count" => 1,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 612.into(), 792.into()],
            }),
        );
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog_id);
        let path = dir.join("hello.pdf");
        doc.save(&path).unwrap();
        path
    }

    #[test]
    fn builtin_text_extraction_reads_each_page() {
        let dir = tempfile::tempdir().unwrap();
        let pages = builtin_page_text(&sample_pdf(dir.path())).unwrap();
        assert_eq!(pages.len(), 1);
        assert!(pages[&1].contains("Hello anytopdf"), "{pages:?}");
        let junk = dir.path().join("junk.pdf");
        fs::write(&junk, b"%PDF-1.4 truncated").unwrap();
        assert!(builtin_page_text(&junk).is_err());
    }

    #[test]
    fn pdf_import_keeps_the_text_layer_with_or_without_poppler() {
        let dir = tempfile::tempdir().unwrap();
        let path = sample_pdf(dir.path());
        let mut registry = Registry::default();
        register_builtins(&mut registry, BuiltinOptions::default());
        let run = Pipeline::new(registry).ingest(&[path], true).unwrap();
        assert_eq!(run.graph.units.len(), 1, "{:?}", run.warnings);
        let unit = &run.graph.units[0];
        assert_eq!(unit.metadata["pdf.page"], "1");
        if which::which("pdftoppm").is_ok() {
            assert_eq!(unit.kind, UnitKind::Visual);
            let text: Vec<&str> = unit
                .annotations
                .iter()
                .filter(|a| a.kind == AnnotationKind::Ocr)
                .map(|a| a.text.as_str())
                .collect();
            assert!(text.join(" ").contains("Hello anytopdf"), "{text:?}");
            // Pages that already carry text are not sent to OCR.
            assert_eq!(
                unit.metadata.get(TEXT_LAYER_KEY).map(String::as_str),
                Some(TEXT_LAYER_NATIVE)
            );
            assert!(
                unit.annotations.iter().all(|a| a.provider != "tesseract"),
                "{:?}",
                unit.annotations
            );
        } else {
            assert_eq!(unit.kind, UnitKind::Text);
            assert!(
                unit.visible_text
                    .as_deref()
                    .unwrap()
                    .contains("Hello anytopdf")
            );
            assert!(
                run.warnings
                    .iter()
                    .any(|d| d.code == DiagnosticCode::ProviderMissing
                        && d.message.contains("pdftoppm")),
                "{:?}",
                run.warnings
            );
        }
    }

    #[test]
    fn pdf_is_claimed_by_magic_and_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.PDF");
        fs::write(&path, b"%PDF-1.7\n").unwrap();
        let source = SourceRecord::new(path);
        assert_eq!(PdfInputImporter.probe(&source), ProbeScore::EXTENSION);
        let mut sniffed = SourceRecord::new(dir.path().join("download"));
        sniffed.detected_type = Some("application/pdf".into());
        assert_eq!(PdfInputImporter.probe(&sniffed), ProbeScore::MAGIC);
    }
}
