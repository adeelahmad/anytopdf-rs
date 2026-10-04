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

pub(crate) fn is_searchable_content(kind: &AnnotationKind) -> bool {
    use AnnotationKind::*;
    matches!(*kind, Ocr | Caption | Transcript | Object | Barcode)
}

pub(crate) fn annotation_line(a: &Annotation) -> String {
    format!("[{:?}][{}] {}", a.kind, a.provider, a.text)
}

pub(crate) fn time_line(t: TimeRange) -> String {
    format!("[TIME] {:.3}-{:.3}s", t.start_seconds, t.end_seconds)
}

// Wrap first, then space every row strictly downward inside the page so rows
// never overlap, wrap around, or get truncated. Spacing and font shrink when crowded.
// Rows are confined below y = height_mm points (the unit the layout is tested in).
pub(crate) fn search_layer(
    lines: &[String],
    width_mm: f32,
    height_mm: f32,
    font: &PdfFontHandle,
) -> Vec<Op> {
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
    let mut ops = Vec::new();
    for (i, row) in rows.into_iter().enumerate() {
        let y_pt = top - step * i as f32;
        ops.extend(hidden_text_ops(
            Point::new(Mm(1.0), Mm(y_pt * 25.4 / 72.0)),
            font.clone(),
            Pt(font_size),
            row,
        ));
    }
    ops
}

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
}
