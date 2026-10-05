//! Q11: text from the graph reaches PDF strings only, never content-stream operators.
use crate::SearchablePdfRenderer;
use anytopdf_core::*;
use printpdf::BuiltinFont;
use std::{fs, path::PathBuf};

const INJECTION: &str = "INJECT-MARKER) Tj ET 0.5 0.25 0.125 rg BT /F1 77 Tf (x\\) Tj <41> Tj ] TJ\r% c\nendstream endobj (";

fn injected_operations(bytes: &[u8]) -> Vec<lopdf::content::Operation> {
    let doc = lopdf::Document::load_mem(bytes).unwrap();
    doc.get_pages()
        .values()
        .flat_map(|page| {
            let content = doc.get_page_content(*page);
            lopdf::content::Content::decode(&content)
                .unwrap()
                .operations
        })
        .collect()
}

#[test]
fn graph_text_cannot_inject_pdf_operators() {
    let dir = tempfile::tempdir().unwrap();
    let font = dir.path().join("font.ttf");
    fs::write(&font, BuiltinFont::Helvetica.get_subset_font().bytes).unwrap();
    let source = SourceRecord::new(PathBuf::from("hostile.txt"));
    let mut unit = Unit::text(source.id, INJECTION.into());
    unit.annotations
        .push(Annotation::text(AnnotationKind::Caption, "test", INJECTION));
    let graph = DocumentGraph {
        units: vec![unit],
        sources: vec![source],
        ..Default::default()
    };
    let ctx = JobContext {
        workspace: dir.path().into(),
        quiet: true,
    };
    for unicode_font in [None, Some(font)] {
        let embedded = unicode_font.is_some();
        let out = dir.path().join(format!("injected-{embedded}.pdf"));
        SearchablePdfRenderer {
            dpi: 144.0,
            unicode_font,
        }
        .render(&ctx, &graph, &out)
        .unwrap();
        let bytes = fs::read(&out).unwrap();
        let operations = injected_operations(&bytes);
        assert!(
            operations
                .iter()
                .any(|op| op.operator == "Tj" || op.operator == "TJ"),
            "embedded={embedded}: no text operators decoded"
        );
        for op in operations {
            let numbers: Vec<f32> = op
                .operands
                .iter()
                .filter_map(|o| o.as_float().ok())
                .collect();
            assert!(
                op.operator != "rg" || numbers != [0.5, 0.25, 0.125],
                "embedded={embedded}: injected colour operator {op:?}"
            );
            assert!(
                op.operator != "Tf" || !numbers.contains(&77.0),
                "embedded={embedded}: injected font operator {op:?}"
            );
        }
        if !embedded {
            let doc = lopdf::Document::load_mem(&bytes).unwrap();
            let text = doc.extract_text(&[1]).unwrap_or_default();
            assert!(text.contains("INJECT-MARKER) Tj ET"), "{text:?}");
        }
    }
}
