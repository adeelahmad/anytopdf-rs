//! Visible boxes over visual pages (`convert --draw-boxes`).
//!
//! Both renderers draw the same overlay: a stroked rectangle per annotation region of
//! the selected kinds and, for faces and objects, a small filled label. The geometry is
//! computed here once, in points with a top-left origin, so the renderers only differ
//! in how they emit the paths. The source image is never modified.
use anytopdf_core::{Annotation, AnnotationKind, DocumentGraph, Unit};
use printpdf::{
    Color, Mm, Op, PaintMode, PdfFontHandle, Point, Pt, Rect, Rgb, TextItem, TextRenderingMode,
};

/// Graph metadata key holding the comma-separated kinds to draw.
pub const DRAW_BOXES_KEY: &str = "anytopdf.draw-boxes";

const MAX_LABEL_CHARS: usize = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BoxKind {
    Ocr,
    Object,
    Face,
}

impl BoxKind {
    pub const ALL: [BoxKind; 3] = [BoxKind::Ocr, BoxKind::Object, BoxKind::Face];

    pub fn as_str(self) -> &'static str {
        match self {
            BoxKind::Ocr => "ocr",
            BoxKind::Object => "objects",
            BoxKind::Face => "faces",
        }
    }

    fn of(kind: &AnnotationKind) -> Option<Self> {
        match kind {
            AnnotationKind::Ocr => Some(BoxKind::Ocr),
            AnnotationKind::Object => Some(BoxKind::Object),
            AnnotationKind::Face => Some(BoxKind::Face),
            _ => None,
        }
    }

    /// Stable RGB colour per kind, dark enough for a white label.
    pub fn rgb(self) -> (u8, u8, u8) {
        match self {
            BoxKind::Ocr => (31, 95, 191),
            BoxKind::Object => (22, 135, 60),
            BoxKind::Face => (204, 85, 0),
        }
    }
}

impl std::str::FromStr for BoxKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "ocr" => Ok(BoxKind::Ocr),
            "objects" | "object" => Ok(BoxKind::Object),
            "faces" | "face" => Ok(BoxKind::Face),
            other => Err(format!(
                "unknown box kind {other:?}; expected objects, faces, ocr or all"
            )),
        }
    }
}

/// Parse a `--draw-boxes` value such as `objects,faces` or `all`; the result is
/// sorted and deduplicated.
pub fn parse_box_kinds(value: &str) -> Result<Vec<BoxKind>, String> {
    let mut kinds = Vec::new();
    for part in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        if part == "all" {
            kinds.extend(BoxKind::ALL);
        } else {
            kinds.push(part.parse()?);
        }
    }
    if kinds.is_empty() {
        return Err("expected at least one of objects, faces, ocr or all".into());
    }
    kinds.sort();
    kinds.dedup();
    Ok(kinds)
}

/// Canonical metadata value for `kinds`, e.g. `ocr,objects,faces`.
pub fn box_kinds_value(kinds: &[BoxKind]) -> String {
    kinds
        .iter()
        .map(|k| k.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// The kinds the graph asks to draw; unknown entries are ignored.
pub(crate) fn requested_kinds(graph: &DocumentGraph) -> Vec<BoxKind> {
    graph
        .metadata
        .get(DRAW_BOXES_KEY)
        .map(|v| {
            let mut kinds: Vec<BoxKind> = v
                .split(',')
                .flat_map(|p| {
                    if p.trim() == "all" {
                        BoxKind::ALL.to_vec()
                    } else {
                        p.parse().into_iter().collect()
                    }
                })
                .collect();
            kinds.sort();
            kinds.dedup();
            kinds
        })
        .unwrap_or_default()
}

/// A rectangle in points, top-left origin.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RectPt {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Label {
    pub text: String,
    /// Filled background behind the text.
    pub rect: RectPt,
    /// Text baseline origin (top-left page origin).
    pub baseline_x: f32,
    pub baseline_y: f32,
    pub size: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OverlayBox {
    pub kind: BoxKind,
    pub rect: RectPt,
    pub stroke: f32,
    pub label: Option<Label>,
}

fn label_text(kind: BoxKind, annotation: &Annotation) -> Option<String> {
    let text = annotation.text.trim();
    let base = match kind {
        BoxKind::Ocr => return None,
        BoxKind::Object => {
            if text.is_empty() {
                "object"
            } else {
                text
            }
        }
        BoxKind::Face => annotation
            .attributes
            .get("person")
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .or(Some(text).filter(|t| !t.is_empty()))
            .unwrap_or("face"),
    };
    let chars: Vec<char> = base.chars().filter(|c| !c.is_control()).collect();
    // ASCII ellipsis: the built-in Helvetica fallback cannot encode U+2026.
    let mut label: String = if chars.len() > MAX_LABEL_CHARS {
        chars[..MAX_LABEL_CHARS - 3]
            .iter()
            .chain(&['.'; 3])
            .collect()
    } else {
        chars.into_iter().collect()
    };
    if let Some(c) = annotation.confidence.filter(|c| c.is_finite()) {
        label.push_str(&format!(" {:.0}%", c.clamp(0.0, 1.0) * 100.0));
    }
    Some(label)
}

/// Lay out the overlay for one visual page of `page_w` x `page_h` points.
/// `text_width` returns a label's advance width in ems. OCR boxes come first and
/// faces last so people stay on top.
pub(crate) fn overlay_boxes(
    kinds: &[BoxKind],
    unit: &Unit,
    page_w: f32,
    page_h: f32,
    text_width: &dyn Fn(&str) -> f32,
) -> Vec<OverlayBox> {
    if kinds.is_empty() || !(page_w > 0.0 && page_h > 0.0) {
        return Vec::new();
    }
    let short = page_w.min(page_h);
    let stroke = (short * 0.004).clamp(0.75, 4.0);
    let size = (short * 0.022).clamp(5.0, 14.0);
    let pad = size * 0.25;
    let label_h = size + 2.0 * pad;
    let mut boxes: Vec<OverlayBox> = unit
        .annotations
        .iter()
        .filter_map(|a| {
            let kind = BoxKind::of(&a.kind).filter(|k| kinds.contains(k))?;
            let r = a.region?.clamped();
            if r.width <= 0.0 || r.height <= 0.0 {
                return None;
            }
            let rect = RectPt {
                x: r.x * page_w,
                y: r.y * page_h,
                w: r.width * page_w,
                h: r.height * page_h,
            };
            let label = label_text(kind, a).map(|text| {
                let w = (text_width(&text) * size + 2.0 * pad).min(page_w);
                let x = rect.x.min(page_w - w).max(0.0);
                // Above the box when it fits, otherwise inside its top edge.
                let y = if rect.y >= label_h {
                    rect.y - label_h
                } else {
                    rect.y.min(page_h - label_h).max(0.0)
                };
                Label {
                    text,
                    rect: RectPt {
                        x,
                        y,
                        w,
                        h: label_h,
                    },
                    baseline_x: x + pad,
                    baseline_y: y + pad + size * 0.8,
                    size,
                }
            });
            Some(OverlayBox {
                kind,
                rect,
                stroke,
                label,
            })
        })
        .collect();
    boxes.sort_by_key(|b| b.kind);
    boxes
}

/// Vector overlay for `--draw-boxes`, drawn over the image and under the hidden text.
pub(crate) fn printpdf_ops(
    graph: &DocumentGraph,
    unit: &Unit,
    page_w_mm: f32,
    page_h_mm: f32,
    font: &PdfFontHandle,
    measure: &dyn Fn(char) -> f32,
) -> Vec<Op> {
    let kinds = requested_kinds(graph);
    let (page_w, page_h) = (Mm(page_w_mm).into_pt().0, Mm(page_h_mm).into_pt().0);
    let width = |text: &str| text.chars().map(measure).sum::<f32>();
    let overlay = overlay_boxes(&kinds, unit, page_w, page_h, &width);
    if overlay.is_empty() {
        return Vec::new();
    }
    let color = |kind: BoxKind| {
        let (r, g, b) = kind.rgb();
        Color::Rgb(Rgb::new(
            r as f32 / 255.0,
            g as f32 / 255.0,
            b as f32 / 255.0,
            None,
        ))
    };
    // printpdf's origin is bottom-left; the overlay's is top-left.
    let rect = |r: RectPt, mode: PaintMode| {
        let mut rect = Rect::from_xywh(Pt(r.x), Pt(page_h - r.y - r.h), Pt(r.w), Pt(r.h));
        rect.mode = Some(mode);
        rect.to_polygon()
    };
    let mut ops = vec![Op::SaveGraphicsState];
    for b in overlay {
        ops.push(Op::SetOutlineColor { col: color(b.kind) });
        ops.push(Op::SetOutlineThickness { pt: Pt(b.stroke) });
        ops.push(Op::DrawPolygon {
            polygon: rect(b.rect, PaintMode::Stroke),
        });
        if let Some(label) = b.label {
            ops.push(Op::SetFillColor { col: color(b.kind) });
            ops.push(Op::DrawPolygon {
                polygon: rect(label.rect, PaintMode::Fill),
            });
            ops.extend([
                Op::SetFillColor {
                    col: Color::Rgb(Rgb::new(1.0, 1.0, 1.0, None)),
                },
                Op::StartTextSection,
                Op::SetTextRenderingMode {
                    mode: TextRenderingMode::Fill,
                },
                Op::SetFont {
                    font: font.clone(),
                    size: Pt(label.size),
                },
                Op::SetTextCursor {
                    pos: Point::new(
                        Mm::from(Pt(label.baseline_x)),
                        Mm::from(Pt(page_h - label.baseline_y)),
                    ),
                },
                Op::ShowText {
                    items: vec![TextItem::Text(label.text)],
                },
                Op::EndTextSection,
            ]);
        }
    }
    ops.push(Op::RestoreGraphicsState);
    ops
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    use anytopdf_core::Region;

    /// Adds a named face and an object to the image unit of [`graph`].
    pub(crate) fn with_detections(mut graph: DocumentGraph, draw: Option<&str>) -> DocumentGraph {
        let image = graph
            .units
            .iter_mut()
            .find(|u| u.visual_path.is_some())
            .unwrap();
        let mut face = Annotation::text(AnnotationKind::Face, "test", "face");
        face.region = Some(Region {
            x: 0.5,
            y: 0.4,
            width: 0.3,
            height: 0.4,
        });
        face.attributes
            .insert("person".into(), "Alicemarker".into());
        let mut object = Annotation::text(AnnotationKind::Object, "test", "dogmarker");
        object.region = Some(Region {
            x: 0.05,
            y: 0.5,
            width: 0.3,
            height: 0.3,
        });
        object.confidence = Some(0.9);
        image.annotations.extend([face, object]);
        if let Some(draw) = draw {
            graph.metadata.insert(DRAW_BOXES_KEY.into(), draw.into());
        }
        graph
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_core::{Region, Uuid};
    use std::path::PathBuf;

    fn region(x: f32, y: f32, w: f32, h: f32) -> Option<Region> {
        Some(Region {
            x,
            y,
            width: w,
            height: h,
        })
    }

    fn unit_with(annotations: Vec<Annotation>) -> Unit {
        let mut unit = Unit::visual(Uuid::new_v4(), PathBuf::from("frame.png"));
        unit.annotations = annotations;
        unit
    }

    fn ann(kind: AnnotationKind, text: &str, r: Option<Region>) -> Annotation {
        let mut a = Annotation::text(kind, "test", text);
        a.region = r;
        a
    }

    fn width(text: &str) -> f32 {
        text.chars().count() as f32 * 0.5
    }

    #[test]
    fn parse_box_kinds_accepts_lists_aliases_and_all() {
        assert_eq!(
            parse_box_kinds("faces,objects"),
            Ok(vec![BoxKind::Object, BoxKind::Face])
        );
        assert_eq!(parse_box_kinds("all"), Ok(BoxKind::ALL.to_vec()));
        assert_eq!(parse_box_kinds("face, face"), Ok(vec![BoxKind::Face]));
        assert!(parse_box_kinds("people").is_err());
        assert!(parse_box_kinds(",").is_err());
        assert_eq!(box_kinds_value(&BoxKind::ALL), "ocr,objects,faces");
    }

    #[test]
    fn requested_kinds_reads_graph_metadata_and_ignores_unknown_entries() {
        let mut graph = DocumentGraph::default();
        assert!(requested_kinds(&graph).is_empty());
        graph
            .metadata
            .insert(DRAW_BOXES_KEY.into(), "faces,bogus,ocr".into());
        assert_eq!(requested_kinds(&graph), vec![BoxKind::Ocr, BoxKind::Face]);
    }

    #[test]
    fn overlay_maps_normalized_regions_to_points_with_top_left_origin() {
        let unit = unit_with(vec![ann(
            AnnotationKind::Object,
            "dog",
            region(0.25, 0.5, 0.5, 0.25),
        )]);
        let boxes = overlay_boxes(&[BoxKind::Object], &unit, 400.0, 200.0, &width);
        assert_eq!(boxes.len(), 1);
        assert_eq!(
            boxes[0].rect,
            RectPt {
                x: 100.0,
                y: 100.0,
                w: 200.0,
                h: 50.0
            }
        );
        let label = boxes[0].label.as_ref().unwrap();
        assert_eq!(label.text, "dog");
        // The label sits directly above the box.
        assert!((label.rect.y + label.rect.h - 100.0).abs() < 1e-3);
        assert_eq!(label.rect.x, 100.0);
    }

    #[test]
    fn overlay_draws_only_selected_kinds_with_regions() {
        let unit = unit_with(vec![
            ann(AnnotationKind::Ocr, "word", region(0.1, 0.1, 0.1, 0.05)),
            ann(AnnotationKind::Object, "cat", region(0.2, 0.2, 0.2, 0.2)),
            ann(AnnotationKind::Face, "face", region(0.5, 0.5, 0.1, 0.1)),
            ann(AnnotationKind::Object, "no region", None),
            ann(
                AnnotationKind::Caption,
                "caption",
                region(0.0, 0.0, 1.0, 1.0),
            ),
            ann(
                AnnotationKind::Object,
                "degenerate",
                region(0.3, 0.3, 0.0, 0.2),
            ),
        ]);
        let kinds = |k: &[BoxKind]| -> Vec<BoxKind> {
            overlay_boxes(k, &unit, 100.0, 100.0, &width)
                .iter()
                .map(|b| b.kind)
                .collect()
        };
        assert!(kinds(&[]).is_empty());
        assert_eq!(kinds(&[BoxKind::Face]), vec![BoxKind::Face]);
        assert_eq!(
            kinds(&BoxKind::ALL),
            vec![BoxKind::Ocr, BoxKind::Object, BoxKind::Face]
        );
        let ocr = overlay_boxes(&[BoxKind::Ocr], &unit, 100.0, 100.0, &width);
        assert!(ocr[0].label.is_none(), "OCR boxes carry no label");
    }

    #[test]
    fn face_label_prefers_matched_person_and_shows_confidence() {
        let mut named = ann(AnnotationKind::Face, "face", region(0.4, 0.4, 0.2, 0.2));
        named.attributes.insert("person".into(), "Alice".into());
        named.confidence = Some(0.873);
        let unknown = ann(AnnotationKind::Face, "", region(0.1, 0.1, 0.2, 0.2));
        let unit = unit_with(vec![named, unknown]);
        let labels: Vec<String> = overlay_boxes(&[BoxKind::Face], &unit, 300.0, 300.0, &width)
            .into_iter()
            .map(|b| b.label.unwrap().text)
            .collect();
        assert_eq!(labels, ["Alice 87%", "face"]);
    }

    #[test]
    fn labels_stay_on_the_page_and_long_text_is_truncated() {
        let long = "x".repeat(100);
        let unit = unit_with(vec![
            ann(AnnotationKind::Object, &long, region(0.9, 0.0, 0.1, 0.1)),
            ann(AnnotationKind::Object, "edge", region(0.0, 0.95, 1.0, 0.05)),
        ]);
        for b in overlay_boxes(&[BoxKind::Object], &unit, 200.0, 100.0, &width) {
            let label = b.label.unwrap();
            assert!(label.text.chars().count() <= MAX_LABEL_CHARS);
            let r = label.rect;
            assert!(r.x >= 0.0 && r.y >= 0.0, "{r:?}");
            assert!(
                r.x + r.w <= 200.0 + 1e-3 && r.y + r.h <= 100.0 + 1e-3,
                "{r:?}"
            );
        }
        let first = &overlay_boxes(&[BoxKind::Object], &unit, 200.0, 100.0, &width)[0];
        let label = first.label.as_ref().unwrap();
        assert!(label.text.ends_with("..."));
        // A box touching the top edge gets its label inside, not above the page.
        assert_eq!(label.rect.y, 0.0);
    }

    #[test]
    fn non_finite_regions_never_escape_the_page() {
        let unit = unit_with(vec![ann(
            AnnotationKind::Face,
            "face",
            region(f32::NAN, 0.5, f32::INFINITY, 2.0),
        )]);
        for b in overlay_boxes(&[BoxKind::Face], &unit, 100.0, 100.0, &width) {
            let r = b.rect;
            assert!(r.x >= 0.0 && r.y >= 0.0 && r.x + r.w <= 100.0 && r.y + r.h <= 100.0);
        }
    }

    #[test]
    fn printpdf_renderer_strokes_labelled_regions_over_the_frame() {
        use crate::SearchablePdfRenderer;
        use anytopdf_core::{JobContext, Renderer, SourceRecord};
        let dir = tempfile::tempdir().unwrap();
        let visual = dir.path().join("frame.png");
        ::image::RgbImage::new(400, 300).save(&visual).unwrap();
        let ops_of = |draw: Option<&str>, name: &str| {
            let source = SourceRecord::new(visual.clone());
            let graph = DocumentGraph {
                units: vec![Unit::visual(source.id, visual.clone())],
                sources: vec![source],
                ..Default::default()
            };
            let graph = fixtures::with_detections(graph, draw);
            let out = dir.path().join(name);
            let ctx = JobContext {
                workspace: dir.path().into(),
                quiet: true,
            };
            SearchablePdfRenderer {
                dpi: 144.0,
                unicode_font: None,
            }
            .render(&ctx, &graph, &out)
            .unwrap();
            let doc = lopdf::Document::load_mem(&std::fs::read(out).unwrap()).unwrap();
            let page = *doc.get_pages().get(&1).unwrap();
            let text = doc.extract_text(&[1]).unwrap_or_default();
            let ops = lopdf::content::Content::decode(&doc.get_page_content(page))
                .unwrap()
                .operations;
            (ops, text)
        };
        let (ops, text) = ops_of(None, "plain.pdf");
        assert!(!ops.iter().any(|op| op.operator == "S"));
        assert!(!text.contains("Alicemarker"));

        let (ops, text) = ops_of(Some("all"), "boxed.pdf");
        assert_eq!(ops.iter().filter(|op| op.operator == "S").count(), 2);
        let image_at = ops.iter().position(|op| op.operator == "Do").unwrap();
        let stroke_at = ops.iter().position(|op| op.operator == "S").unwrap();
        assert!(image_at < stroke_at, "boxes are drawn over the image");
        assert!(text.contains("Alicemarker"), "face label: {text:?}");
        assert!(text.contains("90%"), "object label: {text:?}");
    }
}
