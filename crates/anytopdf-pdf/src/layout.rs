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

/// Hidden search lines a text unit carries on the page where it starts.
pub(crate) fn unit_hidden_lines(unit: &Unit) -> Vec<String> {
    let mut lines: Vec<String> = unit
        .annotations
        .iter()
        .filter(|a| is_searchable_content(&a.kind))
        .map(annotation_line)
        .collect();
    if let Some(time) = unit.time_range {
        lines.push(time_line(time));
    }
    lines
}

/// One visible row of a text page. `unit` indexes the run (`None` for the
/// blank row separating flowed units) and `line` is the source line the row
/// was wrapped from.
pub(crate) struct FlowRow {
    pub unit: Option<usize>,
    pub line: usize,
    pub text: String,
}

#[derive(Default)]
pub(crate) struct FlowPage {
    pub rows: Vec<FlowRow>,
    /// Hidden search lines of the units that start on this page.
    pub hidden: Vec<String>,
    /// Units (run indexes) whose first row is on this page.
    pub starts: Vec<usize>,
}

pub(crate) struct FlowLayout {
    pub pages: Vec<FlowPage>,
    /// First and last page (0-based within the run) of each unit.
    pub unit_pages: Vec<(usize, usize)>,
}

/// Lay out a run of text units: the first starts a new page and each later one
/// (see [`Unit::flows_after`]) continues after a blank row on the same page. A
/// unit that fits on one page moves to a fresh page rather than being split.
pub(crate) fn flow_pages(units: &[&Unit], measure: &impl Fn(char) -> f32) -> FlowLayout {
    let cap = TEXT_ROWS_PER_PAGE;
    let mut pages: Vec<FlowPage> = Vec::new();
    let mut unit_pages = Vec::with_capacity(units.len());
    for (index, unit) in units.iter().enumerate() {
        let text = unit.visible_text.as_deref().unwrap_or("");
        let mut rows: Vec<(usize, String)> = text
            .lines()
            .enumerate()
            .flat_map(|(line, source)| {
                wrap_text(source, TEXT_WRAP_EMS, measure)
                    .into_iter()
                    .map(move |row| (line, row))
            })
            .collect();
        if rows.is_empty() {
            rows.push((0, String::new()));
        }
        let open = pages.last().map_or(0, |p| p.rows.len());
        let fresh =
            index == 0 || open + 2 > cap || (rows.len() <= cap && open + 1 + rows.len() > cap);
        if fresh {
            pages.push(FlowPage::default());
        } else if let Some(page) = pages.last_mut() {
            page.rows.push(FlowRow {
                unit: None,
                line: 0,
                text: String::new(),
            });
        }
        let first = pages.len() - 1;
        if let Some(page) = pages.last_mut() {
            page.starts.push(index);
            page.hidden.extend(unit_hidden_lines(unit));
        }
        for (line, text) in rows {
            if pages.last().is_some_and(|p| p.rows.len() >= cap) {
                pages.push(FlowPage::default());
            }
            if let Some(page) = pages.last_mut() {
                page.rows.push(FlowRow {
                    unit: Some(index),
                    line,
                    text,
                });
            }
        }
        unit_pages.push((first, pages.len() - 1));
    }
    FlowLayout { pages, unit_pages }
}

/// Split `units` into runs that share pages: each run is a maximal sequence in
/// which every unit after the first flows after its predecessor.
pub(crate) fn flow_runs(units: &[Unit]) -> Vec<std::ops::Range<usize>> {
    let mut runs = Vec::new();
    let mut start = 0;
    for i in 1..=units.len() {
        if i == units.len() || !units[i].flows_after(&units[i - 1]) {
            runs.push(start..i);
            start = i;
        }
    }
    runs
}

/// Resolution of a visual unit: print-job pages carry their own in
/// `visual.dpi`; other visuals use the renderer default.
pub(crate) fn unit_dpi(unit: &Unit, default: f32) -> f32 {
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

    fn record(source: Uuid, text: &str) -> Unit {
        let mut unit = Unit::text(source, text.into());
        unit.metadata
            .insert(LAYOUT_FLOW_KEY.into(), LAYOUT_FLOW_CONTINUOUS.into());
        unit
    }

    #[test]
    fn flowed_units_share_pages_and_keep_short_units_whole() {
        let source = Uuid::new_v4();
        let mut units: Vec<Unit> = (0..40)
            .map(|i| record(source, &format!("Record {i}\nid: {i}\nname: n{i}")))
            .collect();
        let mut tagged = Unit::text(source, "plain".into());
        tagged
            .annotations
            .push(Annotation::text(AnnotationKind::Caption, "t", "cue"));
        units.insert(0, tagged);
        assert_eq!(
            flow_runs(&units),
            vec![std::ops::Range { start: 0, end: 41 }]
        );
        let refs: Vec<&Unit> = units.iter().collect();
        let layout = flow_pages(&refs, &|_| 0.5);
        // 59 rows a page: the plain unit and 14 records, then 15, then 11.
        assert_eq!(TEXT_ROWS_PER_PAGE, 59);
        assert_eq!(layout.pages.len(), 3);
        for (index, (first, last)) in layout.unit_pages.iter().enumerate() {
            assert_eq!(first, last, "unit {index} split across pages");
            assert!(layout.pages[*first].starts.contains(&index));
        }
        assert_eq!(layout.pages[0].hidden, ["[Caption][t] cue"]);
        for page in &layout.pages {
            assert!(page.rows.len() <= TEXT_ROWS_PER_PAGE);
            assert!(page.rows.first().is_some_and(|r| r.unit.is_some()));
        }
        let all: Vec<&str> = layout
            .pages
            .iter()
            .flat_map(|p| &p.rows)
            .filter(|r| r.unit.is_some())
            .map(|r| r.text.as_str())
            .collect();
        assert_eq!(all.len(), 121);
        assert_eq!(all[1..4], ["Record 0", "id: 0", "name: n0"]);
    }

    #[test]
    fn flow_runs_break_at_other_sources_visuals_and_unflagged_units() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let units = vec![
            record(a, "1"),
            record(a, "2"),
            Unit::text(a, "3".into()),
            record(b, "4"),
            Unit::visual(b, "v.png".into()),
            record(b, "5"),
        ];
        assert_eq!(flow_runs(&units), [0..2, 2..3, 3..4, 4..5, 5..6]);
        assert!(flow_runs(&[]).is_empty());
    }

    #[test]
    fn long_flowed_unit_splits_across_pages_after_the_open_page() {
        let source = Uuid::new_v4();
        let long = (0..TEXT_ROWS_PER_PAGE * 2)
            .map(|i| format!("k{i}: v"))
            .collect::<Vec<_>>()
            .join("\n");
        let units = [record(source, "short"), record(source, &long)];
        let refs: Vec<&Unit> = units.iter().collect();
        let layout = flow_pages(&refs, &|_| 0.5);
        assert_eq!(layout.unit_pages, [(0, 0), (0, 2)]);
        assert_eq!(layout.pages[0].rows[2].text, "k0: v");
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
