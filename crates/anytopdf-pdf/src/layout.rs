use anytopdf_core::*;
use printpdf::*;

pub(crate) fn hidden_text_ops(pos: Point, font: PdfFontHandle, size: Pt, text: String) -> Vec<Op> {
    vec![
        Op::SaveGraphicsState,
        Op::StartTextSection,
        Op::SetTextRenderingMode {
            mode: TextRenderingMode::Invisible,
        },
        Op::SetTextCursor { pos },
        Op::SetFont { font, size },
        Op::ShowText {
            items: vec![TextItem::Text(text)],
        },
        Op::EndTextSection,
        Op::RestoreGraphicsState,
    ]
}

pub(crate) fn is_searchable_content(a: &Annotation) -> bool {
    use AnnotationKind::*;
    matches!(a.kind, Ocr | Caption | Transcript | Object | Barcode)
        || (matches!(a.kind, Custom | Timestamp) && a.attributes.contains_key("entity"))
}

pub(crate) fn annotation_line(a: &Annotation) -> String {
    match a.attributes.get("iso") {
        // "last Friday (2024-03-01)" is found by searching either form.
        Some(iso) => format!("[{:?}][{}] {} ({iso})", a.kind, a.provider, a.text),
        None => format!("[{:?}][{}] {}", a.kind, a.provider, a.text),
    }
}

pub(crate) fn time_line(t: TimeRange) -> String {
    format!("[TIME] {:.3}-{:.3}s", t.start_seconds, t.end_seconds)
}

// Wrap first, then space every row strictly downward inside the page so rows
// never overlap, wrap around, or get truncated. Spacing and font shrink when crowded.
// Rows are confined below y = height_mm points (the unit the layout is tested in).
// Each row is (baseline in mm from the page bottom, font size in points, text).
pub(crate) fn search_rows(
    lines: &[String],
    width_mm: f32,
    height_mm: f32,
) -> Vec<(f32, f32, String)> {
    const MIN_ROW_COLUMNS: f32 = 64.0;
    const MAX_STEP_PT: f32 = 1.13;
    let row_width = ((width_mm - 3.0).max(0.5) / 25.4 * 72.0).max(MIN_ROW_COLUMNS);
    let rows: Vec<String> = lines
        .iter()
        .flat_map(|line| wrap_text(line, row_width, &|_| 1.0))
        .collect();
    if rows.is_empty() {
        return Vec::new();
    }
    let top = (height_mm - 1.0)
        .min(height_mm / 25.4 * 72.0 - 1.0)
        .max(1.0);
    let step = (top * 0.9 / rows.len() as f32).min(MAX_STEP_PT);
    let font_size = step.min(1.0);
    rows.into_iter()
        .enumerate()
        .map(|(i, row)| ((top - step * i as f32) * 25.4 / 72.0, font_size, row))
        .collect()
}

pub(crate) fn search_layer(
    lines: &[String],
    width_mm: f32,
    height_mm: f32,
    font: &PdfFontHandle,
) -> Vec<Op> {
    search_rows(lines, width_mm, height_mm)
        .into_iter()
        .flat_map(|(y_mm, size, row)| {
            hidden_text_ops(
                Point::new(Mm(SEARCH_X_MM), Mm(y_mm)),
                font.clone(),
                Pt(size),
                row,
            )
        })
        .collect()
}

/// Left edge of the non-positional search layer.
pub(crate) const SEARCH_X_MM: f32 = 1.0;

/// Geometry shared by visible text and provenance pages (A4 portrait).
pub(crate) const TEXT_PAGE_W_MM: f32 = 210.0;
pub(crate) const TEXT_PAGE_H_MM: f32 = 297.0;
pub(crate) const TEXT_MARGIN_MM: f32 = 15.0;
pub(crate) const TEXT_FONT_PT: f32 = 10.0;
pub(crate) const TEXT_LINE_PT: f32 = 13.0;

/// Wrap `text` for a visible text page and split it into page-sized chunks.
pub(crate) fn text_page_chunks(text: &str, measure: &impl Fn(char) -> f32) -> Vec<Vec<String>> {
    wrap_text(text, TEXT_WRAP_EMS, measure)
        .chunks(TEXT_ROWS_PER_PAGE)
        .map(<[String]>::to_vec)
        .collect()
}

/// Line width of a visible text page, in ems of the body font.
pub(crate) const TEXT_WRAP_EMS: f32 =
    (TEXT_PAGE_W_MM - TEXT_MARGIN_MM * 2.0) / 25.4 * 72.0 / TEXT_FONT_PT;
/// Rows that fit on one visible text page.
pub(crate) const TEXT_ROWS_PER_PAGE: usize =
    ((TEXT_PAGE_H_MM - TEXT_MARGIN_MM * 2.0) / 4.5) as usize;

pub(crate) fn wrap_text(text: &str, width: f32, measure: &impl Fn(char) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.lines() {
        let expanded = paragraph.replace('\t', "    ");
        let mut line = String::new();
        let mut used = 0.0;
        for ch in expanded.chars().filter(|c| !c.is_control()) {
            let advance = measure(ch).max(0.0);
            if !line.is_empty() && used + advance > width {
                // Prefer a word boundary, but split long unbroken tokens as well.
                if let Some(boundary) = line.rfind(' ').filter(|i| *i > 0) {
                    let remaining = line[boundary + 1..].to_string();
                    lines.push(line[..boundary].to_string());
                    line = remaining;
                    used = line.chars().map(measure).sum();
                } else {
                    lines.push(std::mem::take(&mut line));
                    used = 0.0;
                }
            }
            line.push(ch);
            used += advance;
        }
        lines.push(line);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Resolution of a visual unit: print-job pages carry their own in
/// `visual.dpi`; other visuals use the renderer default.
pub(crate) fn unit_dpi(unit: &anytopdf_core::Unit, default: f32) -> f32 {
    unit.metadata
        .get("visual.dpi")
        .and_then(|v| v.parse::<f32>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::{helvetica, invisible_items};

    #[test]
    fn wraps_long_tokens_without_losing_characters() {
        let text = "W".repeat(300);
        let lines = wrap_text(&text, 50.0, &|_| 1.0);
        assert_eq!(lines.len(), 6);
        assert_eq!(lines.concat(), text);
    }

    #[test]
    fn wraps_by_glyph_width_and_preserves_indentation() {
        let lines = wrap_text("    abc\n界界界界界", 4.0, &|c| {
            if c == '界' { 2.0 } else { 0.5 }
        });
        assert_eq!(lines[0], "    abc");
        assert_eq!(lines[1..], ["界界", "界界", "界"]);
    }

    #[test]
    fn hidden_text_restores_graphics_state() {
        let ops = hidden_text_ops(
            Point::new(Mm(1.0), Mm(1.0)),
            PdfFontHandle::Builtin(BuiltinFont::Helvetica),
            Pt(1.0),
            "searchable".into(),
        );
        assert!(matches!(ops.first(), Some(Op::SaveGraphicsState)));
        assert!(matches!(ops.last(), Some(Op::RestoreGraphicsState)));
    }

    #[test]
    fn search_layer_rows_never_overlap_or_wrap() {
        let lines: Vec<String> = (0..2000).map(|i| format!("row-{i:04}")).collect();
        let ops = search_layer(&lines, 210.0, 297.0, &helvetica());
        let ys: Vec<f32> = ops
            .iter()
            .filter_map(|op| match op {
                Op::SetTextCursor { pos } => Some(pos.y.0),
                _ => None,
            })
            .collect();
        assert_eq!(ys.len(), 2000, "one cursor per row");
        for pair in ys.windows(2) {
            assert!(pair[1] < pair[0], "y not strictly decreasing: {pair:?}");
        }
        assert!(ys.iter().all(|y| *y > 0.0 && *y < 297.0), "y out of page");
        let shown = invisible_items(&[PdfPage::new(Mm(210.0), Mm(297.0), ops)]);
        assert_eq!(shown[0], lines);
    }

    #[test]
    fn entity_annotations_join_the_search_layer_with_their_iso_value() {
        let mut date = Annotation::text(AnnotationKind::Timestamp, "text-entities", "last Friday");
        date.attributes.insert("entity".into(), "date".into());
        date.attributes.insert("iso".into(), "2024-03-01".into());
        assert!(is_searchable_content(&date));
        assert_eq!(
            annotation_line(&date),
            "[Timestamp][text-entities] last Friday (2024-03-01)"
        );
        let mut url = Annotation::text(AnnotationKind::Custom, "text-entities", "https://x.io");
        url.attributes.insert("entity".into(), "url".into());
        assert!(is_searchable_content(&url));
        // Video frame timestamps and other custom annotations stay out.
        let frame = Annotation::text(AnnotationKind::Timestamp, "ffmpeg-video", "video timestamp");
        assert!(!is_searchable_content(&frame));
        assert!(!is_searchable_content(&Annotation::text(
            AnnotationKind::Custom,
            "p",
            "x"
        )));
    }
}
