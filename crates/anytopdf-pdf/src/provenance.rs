use crate::layout::{
    TEXT_FONT_PT, TEXT_LINE_PT, TEXT_MARGIN_MM, TEXT_PAGE_H_MM, TEXT_PAGE_W_MM, text_page_chunks,
};
use anytopdf_core::*;
use printpdf::*;
use std::collections::BTreeMap;

pub(crate) fn provenance_lines(
    graph: &DocumentGraph,
    unit_pages: &BTreeMap<Uuid, PageRange>,
) -> Vec<String> {
    let mut lines = vec!["Provenance".to_string(), String::new()];
    if let Some(profile) = graph.metadata.get("anytopdf.profile") {
        lines.push(format!("Profile: {profile}"));
    }
    if let Some(t) = graph
        .metadata
        .get("anytopdf.created")
        .and_then(|v| v.parse::<i64>().ok())
        .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
    {
        lines.push(format!(
            "Created: {:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            t.year(),
            t.month() as u8,
            t.day(),
            t.hour(),
            t.minute(),
            t.second()
        ));
    }
    let mut providers: BTreeMap<String, String> = BTreeMap::new();
    for (key, version) in &graph.metadata {
        if let Some(name) = provider_name(key) {
            providers.insert(name.to_string(), format!("{name} {version}"));
        }
    }
    for annotation in graph.units.iter().flat_map(|u| &u.annotations) {
        providers
            .entry(annotation.provider.clone())
            .or_insert_with(|| annotation.provider.clone());
    }
    lines.push("Providers:".into());
    lines.extend(providers.into_values().map(|p| format!("  {p}")));
    for source in &graph.sources {
        lines.push(String::new());
        let name = basename(&source.path);
        lines.push(format!("Source: {name}"));
        if let Some(hash) = &source.sha256 {
            lines.push(format!("SHA-256: {hash}"));
        }
        if let Some(size) = source.size {
            lines.push(format!("Size: {size} bytes"));
        }
        if let Some(ty) = &source.detected_type {
            lines.push(format!("Type: {ty}"));
        }
        let (mut first, mut last) = (usize::MAX, 0);
        for range in graph
            .units
            .iter()
            .filter(|u| u.source_id == source.id)
            .filter_map(|u| unit_pages.get(&u.id))
        {
            first = first.min(range.first);
            last = last.max(range.last);
        }
        if last > 0 {
            lines.push(format!("Pages: {first}–{last}"));
        }
    }
    lines
}

pub(crate) fn provenance_pages(
    graph: &DocumentGraph,
    unit_pages: &BTreeMap<Uuid, PageRange>,
    font: &PdfFontHandle,
    measure: &impl Fn(char) -> f32,
) -> Vec<PdfPage> {
    let (page_w, page_h, margin) = (TEXT_PAGE_W_MM, TEXT_PAGE_H_MM, TEXT_MARGIN_MM);
    text_page_chunks(&provenance_lines(graph, unit_pages).join("\n"), measure)
        .into_iter()
        .map(|chunk| {
            let mut ops = vec![
                Op::StartTextSection,
                Op::SetTextRenderingMode {
                    mode: TextRenderingMode::Fill,
                },
                Op::SetFont {
                    font: font.clone(),
                    size: Pt(TEXT_FONT_PT),
                },
                Op::SetLineHeight {
                    lh: Pt(TEXT_LINE_PT),
                },
                Op::SetTextCursor {
                    pos: Point::new(Mm(margin), Mm(page_h - margin)),
                },
            ];
            for line in chunk {
                ops.push(Op::ShowText {
                    items: vec![TextItem::Text(line.clone())],
                });
                ops.push(Op::AddLineBreak);
            }
            ops.push(Op::EndTextSection);
            PdfPage::new(Mm(page_w), Mm(page_h), ops)
        })
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tests::helvetica;
    use std::path::PathBuf;

    pub(crate) fn prov_source(name: &str, hash: char, size: u64, ty: &str) -> SourceRecord {
        let mut s = SourceRecord::new(PathBuf::from(format!("/home/user/private/{name}")));
        s.sha256 = Some(hash.to_string().repeat(64));
        s.size = Some(size);
        s.detected_type = Some(ty.into());
        s
    }

    fn prov_graph() -> (DocumentGraph, BTreeMap<Uuid, PageRange>) {
        let a = prov_source("notes.txt", 'a', 17, "text/plain");
        let b = prov_source("scan.png", 'b', 99, "image/png");
        let ua = Unit::text(a.id, "hello".into());
        let mut ub = Unit::text(b.id, "scan".into());
        ub.annotations.push(Annotation::text(
            AnnotationKind::Ocr,
            "tesseract",
            "scan words",
        ));
        let mut graph = DocumentGraph {
            units: vec![ua.clone(), ub.clone()],
            sources: vec![a, b],
            ..Default::default()
        };
        for (k, v) in [
            ("anytopdf.profile", "share"),
            ("provider.tesseract.version", "5.5.0"),
            ("anytopdf.created", "1700000000"),
        ] {
            graph.metadata.insert(k.into(), v.into());
        }
        let map = BTreeMap::from([
            (ua.id, PageRange { first: 1, last: 1 }),
            (ub.id, PageRange { first: 2, last: 3 }),
        ]);
        (graph, map)
    }

    #[test]
    fn provenance_lists_every_source_with_hash_size_type_providers_profile_and_pages() {
        let (graph, map) = prov_graph();
        let lines = provenance_lines(&graph, &map);
        let has = |needle: &str| lines.iter().any(|l| l.contains(needle));
        assert!(has("Profile: share"), "{lines:?}");
        assert!(has("Created: 2023-11-14T22:13:20Z"), "{lines:?}");
        for (name, hash, size, pages) in
            [("notes.txt", 'a', 17, "1–1"), ("scan.png", 'b', 99, "2–3")]
        {
            assert!(has(&format!("Source: {name}")), "{name}: {lines:?}");
            assert!(
                has(&format!("SHA-256: {}", hash.to_string().repeat(64))),
                "{lines:?}"
            );
            assert!(has(&format!("Size: {size} bytes")), "{lines:?}");
            assert!(has(&format!("Pages: {pages}")), "{lines:?}");
        }
        assert!(has("Providers:"), "{lines:?}");
        assert!(has("tesseract 5.5.0"), "{lines:?}");
        // "Type: text/plain" legitimately carries a slash; every other line must not.
        assert!(
            lines
                .iter()
                .filter(|l| !l.starts_with("Type:"))
                .all(|l| !l.contains('/')),
            "path separator leaked: {lines:?}"
        );
        assert!(!has("/home"), "{lines:?}");
    }

    #[test]
    fn provenance_pages_are_visible_text() {
        let (graph, map) = prov_graph();
        let pages = provenance_pages(&graph, &map, &helvetica(), &|_| 1.0);
        assert!(!pages.is_empty(), "no provenance pages produced");
        let ops: Vec<&Op> = pages.iter().flat_map(|p| p.ops.iter()).collect();
        assert!(ops.iter().any(|o| matches!(
            o,
            Op::SetTextRenderingMode {
                mode: TextRenderingMode::Fill
            }
        )));
        assert!(!ops.iter().any(|o| matches!(
            o,
            Op::SetTextRenderingMode {
                mode: TextRenderingMode::Invisible
            }
        )));
        assert!(ops.iter().any(|o| matches!(
            o,
            Op::ShowText { items } if items.iter().any(
                |i| matches!(i, TextItem::Text(t) if t.contains("SHA-256:")))
        )));
    }
}
