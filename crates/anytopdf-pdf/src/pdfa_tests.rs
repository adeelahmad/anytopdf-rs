use super::*;
use crate::{SearchablePdfRenderer, read_embedded_files};

// printpdf's bundled Helvetica subset: a real TrueType font present on every CI host.
fn fixture_font(dir: &Path) -> PathBuf {
    let path = dir.join("fixture.ttf");
    fs::write(
        &path,
        printpdf::BuiltinFont::Helvetica.get_subset_font().bytes,
    )
    .unwrap();
    path
}

fn renderer(dir: &Path) -> PdfARenderer {
    PdfARenderer {
        dpi: 144.0,
        unicode_font: Some(fixture_font(dir)),
        fallback_fonts: Vec::new(),
    }
}

fn ctx(dir: &Path) -> JobContext {
    JobContext {
        workspace: dir.into(),
        quiet: true,
    }
}

fn graph(dir: &Path) -> DocumentGraph {
    let visual = dir.join("image.png");
    ::image::RgbImage::new(40, 30).save(&visual).unwrap();
    let text_source = SourceRecord::new(dir.join("notes.txt"));
    let mut image_source = SourceRecord::new(visual.clone());
    image_source
        .metadata
        .insert("exiftool.GPS:GPSLatitude".into(), "51.5".into());
    let mut image = Unit::visual(image_source.id, visual);
    image.time_range = Some(TimeRange {
        start_seconds: 1.0,
        end_seconds: 2.0,
    });
    image.annotations.push(Annotation::text(
        AnnotationKind::Caption,
        "test",
        "captionmarker",
    ));
    let mut ocr = Annotation::text(AnnotationKind::Ocr, "test", "ocrmarker");
    ocr.region = Some(Region {
        x: 0.1,
        y: 0.2,
        width: 0.5,
        height: 0.1,
    });
    image.annotations.push(ocr);
    let mut graph = DocumentGraph {
        units: vec![
            Unit::text(text_source.id, "visiblemarker\nsecond".into()),
            image,
        ],
        sources: vec![text_source, image_source],
        ..Default::default()
    };
    graph
        .metadata
        .insert("anytopdf.created".into(), "1700000000".into());
    graph
}

fn render(dir: &Path, graph: &DocumentGraph, name: &str) -> (Vec<u8>, RenderReport) {
    let out = dir.join(name);
    let report = renderer(dir).render(&ctx(dir), graph, &out).unwrap();
    (fs::read(out).unwrap(), report)
}

fn page_texts(bytes: &[u8]) -> Vec<String> {
    let doc = lopdf::Document::load_mem(bytes).unwrap();
    doc.get_pages()
        .keys()
        .map(|n| doc.extract_text(&[*n]).unwrap_or_default())
        .collect()
}

#[test]
fn declares_pdfa3a_with_output_intent_and_xmp() {
    let dir = tempfile::tempdir().unwrap();
    let (bytes, _) = render(dir.path(), &graph(dir.path()), "a.pdf");
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let catalog = doc.catalog().unwrap();
    assert!(catalog.get(b"OutputIntents").is_ok(), "no output intent");
    // PDF/A keeps the XMP packet uncompressed so it stays readable.
    let xmp = &doc
        .get_object(catalog.get(b"Metadata").unwrap().as_reference().unwrap())
        .unwrap()
        .as_stream()
        .unwrap()
        .content;
    let xmp = String::from_utf8_lossy(xmp);
    assert!(xmp.contains("<pdfaid:part>3</pdfaid:part>"), "{xmp}");
    assert!(
        xmp.contains("<pdfaid:conformance>A</pdfaid:conformance>"),
        "{xmp}"
    );
    assert!(xmp.contains("2023-11-14T22:13:20"), "{xmp}");
}

#[test]
fn manifest_and_chunks_are_associated_files_matching_the_report() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let (bytes, report) = render(dir.path(), &graph, "a.pdf");
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let af = doc
        .catalog()
        .unwrap()
        .get(b"AF")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(af.len(), 2);
    for spec in af {
        let spec = doc.dereference(spec).unwrap().1.as_dict().unwrap();
        assert_eq!(
            spec.get(b"AFRelationship").unwrap().as_name().unwrap(),
            b"Data"
        );
        assert!(spec.get(b"Desc").is_ok() && spec.get(b"UF").is_ok());
    }
    let files = read_embedded_files(&bytes).unwrap();
    let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, [CHUNKS_FILE, MANIFEST_FILE]);
    let manifest = serde_json::to_vec_pretty(&Manifest::build(&graph, &report)).unwrap();
    let chunks = serde_json::to_vec_pretty(&ChunkSet::build(&graph, &report)).unwrap();
    assert!(files[0].bytes == chunks, "chunks differ from the report");
    assert!(
        files[1].bytes == manifest,
        "manifest differs from the report"
    );
    assert!(files.iter().all(|f| f.mime_type == "application/json"));
}

#[test]
fn same_graph_renders_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let (a, _) = render(dir.path(), &graph, "a.pdf");
    let (b, _) = render(dir.path(), &graph, "b.pdf");
    assert!(a == b, "same graph rendered to different bytes");
}

#[test]
fn page_mapping_matches_the_printpdf_renderer() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = graph(dir.path());
    let long: String = (0..150).map(|i| format!("row{i:03}\n")).collect();
    graph.units[0].visible_text = Some(long);
    let (_, ours) = render(dir.path(), &graph, "a.pdf");
    let theirs = SearchablePdfRenderer {
        dpi: 144.0,
        unicode_font: Some(fixture_font(dir.path())),
    }
    .render(&ctx(dir.path()), &graph, &dir.path().join("b.pdf"))
    .unwrap();
    assert_eq!(ours.pages, theirs.pages);
    assert_eq!(ours.unit_pages, theirs.unit_pages);
}

#[test]
fn hidden_layer_is_transparent_content_without_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let (bytes, _) = render(dir.path(), &graph, "a.pdf");
    let texts = page_texts(&bytes);
    assert!(texts[0].contains("visiblemarker"), "{:?}", texts[0]);
    let visual = &texts[1];
    for marker in ["captionmarker", "ocrmarker", "[TIME]"] {
        assert!(visual.contains(marker), "{marker} missing: {visual:?}");
    }
    for noise in ["GPSLatitude", "[META]", "[SOURCE]", "image.png"] {
        assert!(!visual.contains(noise), "{noise} leaked: {visual:?}");
    }
    // Every glyph on the image page is drawn under a zero fill alpha.
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let page = *doc.get_pages().get(&2).unwrap();
    let (inline, referenced) = doc.get_page_resources(page).unwrap();
    let resources = inline
        .or_else(|| {
            referenced
                .first()
                .and_then(|id| doc.get_dictionary(*id).ok())
        })
        .unwrap();
    let states = resources.get(b"ExtGState").unwrap();
    let states = doc.dereference(states).unwrap().1.as_dict().unwrap();
    assert!(
        states
            .iter()
            .any(|(_, gs)| doc.dereference(gs).ok().and_then(|(_, gs)| gs
                .as_dict()
                .ok()?
                .get(b"ca")
                .ok()?
                .as_float()
                .ok())
                == Some(0.0)),
        "no transparent fill state on the image page"
    );
}

#[test]
fn glyphs_missing_from_the_font_are_dropped_with_a_warning() {
    let dir = tempfile::tempdir().unwrap();
    let mut graph = graph(dir.path());
    graph.units[0].visible_text = Some("abc 漢字 xyz".into());
    let (bytes, report) = render(dir.path(), &graph, "a.pdf");
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("lacks 2 character")),
        "{:?}",
        report.warnings
    );
    let text = &page_texts(&bytes)[0];
    assert!(text.contains("abc") && text.contains("xyz"), "{text:?}");
}

#[test]
fn unreadable_font_fails_without_touching_destination() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("existing.pdf");
    fs::write(&output, b"keep").unwrap();
    let err = PdfARenderer {
        dpi: 144.0,
        unicode_font: Some(dir.path().join("missing.ttf")),
        fallback_fonts: Vec::new(),
    }
    .render(&ctx(dir.path()), &graph(dir.path()), &output)
    .unwrap_err();
    assert!(err.to_string().contains("missing.ttf"), "{err:#}");
    assert_eq!(fs::read(output).unwrap(), b"keep");
}

#[test]
fn bundled_font_is_embedded_when_none_is_configured() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.pdf");
    PdfARenderer {
        dpi: 144.0,
        unicode_font: None,
        fallback_fonts: Vec::new(),
    }
    .render(&ctx(dir.path()), &graph(dir.path()), &output)
    .unwrap();
    let doc = lopdf::Document::load(&output).unwrap();
    let names: Vec<String> = doc
        .objects
        .values()
        .filter_map(|o| o.as_dict().ok())
        .filter_map(|d| d.get(b"BaseFont").ok())
        .filter_map(|n| n.as_name().ok())
        .map(|n| String::from_utf8_lossy(n).into_owned())
        .collect();
    assert!(names.iter().any(|n| n.ends_with("DejaVuSans")), "{names:?}");
}

#[test]
fn empty_graph_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("x.pdf");
    assert!(
        renderer(dir.path())
            .render(&ctx(dir.path()), &DocumentGraph::default(), &out)
            .is_err()
    );
    assert!(!out.exists());
}

#[test]
fn structure_tree_tags_figures_paragraphs_and_the_provenance_heading() {
    let dir = tempfile::tempdir().unwrap();
    let (bytes, _) = render(dir.path(), &graph(dir.path()), "a.pdf");
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let catalog = doc.catalog().unwrap();
    assert!(catalog.get(b"StructTreeRoot").is_ok(), "no structure tree");
    let marked = catalog.get(b"MarkInfo").unwrap();
    let marked = doc.dereference(marked).unwrap().1.as_dict().unwrap();
    assert!(marked.get(b"Marked").unwrap().as_bool().unwrap());
    let kinds: Vec<(Vec<u8>, Option<String>)> = doc
        .objects
        .values()
        .filter_map(|o| o.as_dict().ok())
        .filter(|d| d.get(b"Type").and_then(|t| t.as_name()).ok() == Some(b"StructElem"))
        .map(|d| {
            let kind = d.get(b"S").unwrap().as_name().unwrap().to_vec();
            let alt = d
                .get(b"Alt")
                .ok()
                .and_then(|a| a.as_str().ok())
                .map(|a| String::from_utf8_lossy(a).into_owned());
            (kind, alt)
        })
        .collect();
    let has = |k: &[u8]| kinds.iter().any(|(kind, _)| kind == k);
    assert!(has(b"P") && has(b"H1") && has(b"Sect"), "{kinds:?}");
    assert!(
        kinds.iter().any(|(k, alt)| k == b"Figure"
            && alt.as_deref() == Some("Video frame at 1.000 to 2.000 seconds")),
        "{kinds:?}"
    );
}

#[test]
fn bookmarks_point_at_each_source_and_the_provenance_page() {
    let dir = tempfile::tempdir().unwrap();
    let (bytes, report) = render(dir.path(), &graph(dir.path()), "a.pdf");
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let pages: Vec<lopdf::ObjectId> = doc.get_pages().values().copied().collect();
    let mut item = doc
        .get_dictionary(
            doc.catalog()
                .unwrap()
                .get(b"Outlines")
                .unwrap()
                .as_reference()
                .unwrap(),
        )
        .unwrap()
        .get(b"First")
        .unwrap()
        .as_reference()
        .ok();
    let mut found = Vec::new();
    while let Some(id) = item {
        let entry = doc.get_dictionary(id).unwrap();
        let title = String::from_utf8_lossy(entry.get(b"Title").unwrap().as_str().unwrap());
        let dest = doc
            .dereference(entry.get(b"Dest").unwrap())
            .unwrap()
            .1
            .as_array()
            .unwrap();
        let page = pages
            .iter()
            .position(|p| *p == dest[0].as_reference().unwrap())
            .unwrap();
        found.push((title.into_owned(), page + 1));
        item = entry.get(b"Next").and_then(|n| n.as_reference()).ok();
    }
    assert_eq!(
        found,
        [
            ("notes.txt".to_string(), 1),
            ("image.png".to_string(), 2),
            ("Provenance".to_string(), report.pages)
        ]
    );
}

#[test]
fn fallback_fonts_cover_characters_the_primary_font_lacks() {
    let dir = tempfile::tempdir().unwrap();
    // The primary font keeps only ASCII; the full fixture is the fallback.
    let full = fixture_font(dir.path());
    let ascii = dir.path().join("ascii.ttf");
    let bytes = fs::read(&full).unwrap();
    fs::write(
        &ascii,
        crate::fonts::subset_document_font(&bytes, &DocumentGraph::default()).unwrap(),
    )
    .unwrap();
    let mut graph = graph(dir.path());
    graph.units[0].visible_text = Some("abc àéç xyz".into());
    let out = dir.path().join("a.pdf");
    let report = PdfARenderer {
        dpi: 144.0,
        unicode_font: Some(ascii),
        fallback_fonts: vec![full],
    }
    .render(&ctx(dir.path()), &graph, &out)
    .unwrap();
    assert!(
        report.warnings.iter().all(|w| !w.contains("lacks")),
        "{:?}",
        report.warnings
    );
    let bytes = fs::read(out).unwrap();
    let text = &page_texts(&bytes)[0];
    assert!(text.contains('é') && text.contains("xyz"), "{text:?}");
    let fonts = String::from_utf8_lossy(&bytes)
        .matches("/FontFile2")
        .count();
    assert_eq!(fonts, 2, "expected the primary and one fallback font");
}

#[test]
fn visual_dpi_metadata_sets_the_physical_page_size() {
    let dir = tempfile::tempdir().unwrap();
    let visual = dir.path().join("page.png");
    ::image::GrayImage::new(600, 300).save(&visual).unwrap();
    let source = SourceRecord::new(visual.clone());
    let mut unit = Unit::visual(source.id, visual);
    unit.metadata.insert("visual.dpi".into(), "300".into());
    let graph = DocumentGraph {
        units: vec![unit],
        sources: vec![source],
        ..Default::default()
    };
    let (bytes, _) = render(dir.path(), &graph, "dpi.pdf");
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let first = *doc.get_pages().values().next().unwrap();
    let media = doc
        .get_object(first)
        .and_then(lopdf::Object::as_dict)
        .and_then(|page| page.get(b"MediaBox"))
        .and_then(lopdf::Object::as_array)
        .unwrap();
    let size: Vec<f32> = media[2..]
        .iter()
        .map(|v| v.as_float().unwrap().round())
        .collect();
    // 600 x 300 pixels at 300 dpi is 2 x 1 inches.
    assert_eq!(size, [144.0, 72.0]);
}

#[test]
fn ocr_words_are_stretched_to_their_boxes() {
    assert_eq!(ocr_stretch(20.0, 10.0), 2.0);
    assert_eq!(ocr_stretch(100.0, 1.0), 4.0);
    assert_eq!(ocr_stretch(1.0, 100.0), 0.25);
    assert_eq!(ocr_stretch(0.0, 10.0), 1.0);
    assert_eq!(ocr_stretch(10.0, 0.0), 1.0);

    let dir = tempfile::tempdir().unwrap();
    let graph = graph(dir.path());
    let (bytes, _) = render(dir.path(), &graph, "a.pdf");
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let page = *doc.get_pages().get(&2).unwrap();
    let content = lopdf::content::Content::decode(&doc.get_page_content(page)).unwrap();
    let ops = &content.operations;
    let text_at = ops
        .iter()
        .position(|op| op.operator == "Tj" || op.operator == "TJ");
    let numbers = |op: &lopdf::content::Operation| -> Vec<f32> {
        op.operands.iter().map(|o| o.as_float().unwrap()).collect()
    };
    // The OCR word's horizontal scale is set by a `cm` just before its text.
    let cm = ops[..text_at.expect("no text drawn on the image page")]
        .iter()
        .rev()
        .find(|op| op.operator == "cm")
        .map(numbers)
        .expect("OCR word has no stretch transform");
    // krilla folds its y-down page flip into the same matrix.
    assert_eq!((cm[1], cm[2], cm[3].abs()), (0.0, 0.0, 1.0), "{cm:?}");
    assert!(cm[0] > 0.0 && (cm[0] - 1.0).abs() > 0.01, "{cm:?}");
}

use crate::boxes::fixtures::with_detections;

fn image_page_ops(bytes: &[u8]) -> Vec<lopdf::content::Operation> {
    let doc = lopdf::Document::load_mem(bytes).unwrap();
    let page = *doc.get_pages().get(&2).unwrap();
    lopdf::content::Content::decode(&doc.get_page_content(page))
        .unwrap()
        .operations
}

#[test]
fn draw_boxes_strokes_labelled_artifacts_over_the_frame() {
    let dir = tempfile::tempdir().unwrap();
    let plain = with_detections(graph(dir.path()), None);
    let (bytes, report) = render(dir.path(), &plain, "plain.pdf");
    assert!(
        !image_page_ops(&bytes).iter().any(|op| op.operator == "S"),
        "boxes drawn without --draw-boxes"
    );
    assert!(!page_texts(&bytes)[1].contains("Alicemarker"));

    let boxed = with_detections(graph(dir.path()), Some("objects,faces"));
    let (bytes, boxed_report) = render(dir.path(), &boxed, "boxed.pdf");
    // The overlay adds no pages and keeps the document valid PDF/A-3a (krilla
    // validates on export).
    assert_eq!(report.pages, boxed_report.pages);
    let ops = image_page_ops(&bytes);
    let strokes = ops.iter().filter(|op| op.operator == "S").count();
    assert_eq!(strokes, 2, "one stroked rectangle per selected detection");
    let image_at = ops.iter().position(|op| op.operator == "Do").unwrap();
    let stroke_at = ops.iter().position(|op| op.operator == "S").unwrap();
    assert!(image_at < stroke_at, "boxes are drawn over the image");
    assert!(
        ops.iter()
            .any(|op| (op.operator == "BDC" || op.operator == "BMC")
                && op.operands.first().and_then(|o| o.as_name().ok()) == Some(b"Artifact")),
        "overlay must be tagged as an artifact"
    );
    let text = &page_texts(&bytes)[1];
    assert!(text.contains("Alicemarker"), "face label: {text:?}");
    assert!(text.contains("90%"), "object label: {text:?}");

    // Only the selected kinds are drawn.
    let faces = with_detections(graph(dir.path()), Some("faces"));
    let (bytes, _) = render(dir.path(), &faces, "faces.pdf");
    let strokes = image_page_ops(&bytes)
        .iter()
        .filter(|op| op.operator == "S")
        .count();
    assert_eq!(strokes, 1);
}
