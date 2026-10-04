use anyhow::{Result, bail};
use anytopdf_core::*;
use printpdf::*;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub struct SearchablePdfRenderer {
    pub dpi: f32,
    pub unicode_font: Option<PathBuf>,
}

impl Default for SearchablePdfRenderer {
    fn default() -> Self {
        Self {
            dpi: 144.0,
            unicode_font: find_system_font(),
        }
    }
}

impl Plugin for SearchablePdfRenderer {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "pdf".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "renderer".into(),
            extensions: vec!["pdf".into()],
            mime_types: vec!["application/pdf".into()],
            priority: 100,
        }
    }
}

impl Renderer for SearchablePdfRenderer {
    fn render(
        &self,
        _ctx: &JobContext,
        graph: &DocumentGraph,
        output: &Path,
    ) -> Result<RenderReport> {
        if !self.dpi.is_finite() || self.dpi <= 0.0 {
            bail!("PDF DPI must be finite and positive");
        }
        let mut doc = PdfDocument::new("anytopdf");
        let mut pdf_warnings = Vec::new();
        let mut own_warnings = Vec::new();
        let mut pages = Vec::new();

        let font_bytes = self.unicode_font.as_ref().map(fs::read).transpose()?;
        let face = font_bytes
            .as_deref()
            .map(|b| ttf_parser::Face::parse(b, 0))
            .transpose()?;
        // printpdf without its full HTML/text-layout engine embeds whole fonts.
        // Subset first so a small PDF does not embed a multi-megabyte system font.
        let subset_bytes = font_bytes
            .as_deref()
            .map(|bytes| subset_document_font(bytes, graph))
            .transpose()?;
        let font_handle = self.load_font(&mut doc, subset_bytes.as_deref())?;
        if face.is_none() {
            own_warnings.push("No Unicode font found; using Helvetica with limited character coverage. Set ANYTOPDF_FONT to a TTF font.".into());
        }
        if let Some(face) = &face {
            let missing: std::collections::BTreeSet<char> = graph
                .units
                .iter()
                .flat_map(|u| {
                    u.visible_text
                        .iter()
                        .map(String::as_str)
                        .chain(u.annotations.iter().map(|a| a.text.as_str()))
                })
                .flat_map(str::chars)
                .filter(|c| !c.is_control() && face.glyph_index(*c).is_none())
                .collect();
            if !missing.is_empty() {
                own_warnings.push(format!("Font lacks {} character(s): {:?}. Set ANYTOPDF_FONT to a font covering this script.", missing.len(), missing.iter().take(20).collect::<String>()));
            }
        }
        let measure = |ch: char| -> f32 {
            face.as_ref()
                .and_then(|f| f.glyph_index(ch).and_then(|g| f.glyph_hor_advance(g)))
                .map(|w| w as f32 / face.as_ref().unwrap().units_per_em() as f32)
                .unwrap_or(1.0)
        };

        for unit in &graph.units {
            if let Some(visual) = &unit.visual_path {
                match self.visual_page(
                    &mut doc,
                    graph,
                    unit,
                    visual,
                    &font_handle,
                    &mut pdf_warnings,
                ) {
                    Ok(page) => pages.push(page),
                    Err(e) => {
                        own_warnings.push(format!("visual page {} failed: {e:#}", visual.display()))
                    }
                }
            } else if let Some(text) = &unit.visible_text {
                pages.extend(self.text_pages(graph, unit, text, &font_handle, &measure));
            }
        }

        if pages.is_empty() {
            anyhow::bail!("no PDF pages generated");
        }

        let page_count = pages.len();
        doc.with_pages(pages);
        let mut save_warnings = Vec::new();
        let bytes = doc.save(
            &PdfSaveOptions {
                subset_fonts: true,
                ..Default::default()
            },
            &mut save_warnings,
        );

        atomic_write(output, &bytes)?;

        let mut warning_text = own_warnings;
        warning_text.extend(
            pdf_warnings
                .into_iter()
                .chain(save_warnings)
                .filter(|w| w.severity != PdfParseErrorSeverity::Info)
                .map(|w| format!("{w:?}")),
        );

        Ok(RenderReport {
            pages: page_count,
            warnings: warning_text,
        })
    }
}

impl SearchablePdfRenderer {
    fn load_font(&self, doc: &mut PdfDocument, bytes: Option<&[u8]>) -> Result<PdfFontHandle> {
        if let Some(bytes) = bytes {
            let mut warnings = Vec::new();
            let font = ParsedFont::from_bytes(bytes, 0, &mut warnings)
                .ok_or_else(|| anyhow::anyhow!("could not parse Unicode font"))?;
            return Ok(PdfFontHandle::External(doc.add_font(&font)));
        }
        Ok(PdfFontHandle::Builtin(BuiltinFont::Helvetica))
    }

    fn visual_page(
        &self,
        doc: &mut PdfDocument,
        _graph: &DocumentGraph,
        unit: &Unit,
        visual: &Path,
        font: &PdfFontHandle,
        warnings: &mut Vec<PdfWarnMsg>,
    ) -> Result<PdfPage> {
        let bytes = fs::read(visual)?;
        let raw = RawImage::decode_from_bytes(&bytes, warnings)
            .map_err(|e| anyhow::anyhow!("decode image: {e}"))?;
        let dimensions = ::image::ImageReader::open(visual)?
            .with_guessed_format()?
            .into_dimensions()?;
        let page_w_mm = dimensions.0 as f32 / self.dpi * 25.4;
        let page_h_mm = dimensions.1 as f32 / self.dpi * 25.4;

        let image_id = doc.add_image(&raw);
        let mut ops = vec![Op::UseXobject {
            id: image_id,
            transform: XObjectTransform {
                dpi: Some(self.dpi),
                ..Default::default()
            },
        }];

        // Positioned OCR layer.
        for annotation in unit
            .annotations
            .iter()
            .filter(|a| a.kind == AnnotationKind::Ocr)
        {
            if let Some(region) = annotation.region {
                let r = region.clamped();
                let x = page_w_mm * r.x;
                let y_top = page_h_mm * r.y;
                let h = (page_h_mm * r.height).max(1.0);
                let baseline_y = (page_h_mm - y_top - h * 0.85).max(0.5);
                let font_pt = ((h / 25.4) * 72.0 * 0.78).clamp(3.0, 72.0);

                ops.extend(hidden_text_ops(
                    Point::new(Mm(x), Mm(baseline_y)),
                    font.clone(),
                    Pt(font_pt),
                    annotation.text.clone(),
                ));
            }
        }

        // Non-positional searchable layer: content annotations only, never
        // source paths or file metadata.
        let mut search_lines = Vec::new();
        if let Some(t) = unit.time_range {
            search_lines.push(time_line(t));
        }
        search_lines.extend(
            unit.annotations
                .iter()
                .filter(|a| {
                    is_searchable_content(&a.kind)
                        && (a.kind != AnnotationKind::Ocr || a.region.is_none())
                })
                .map(annotation_line),
        );

        ops.extend(search_layer(&search_lines, page_w_mm, page_h_mm, font));

        Ok(PdfPage::new(Mm(page_w_mm), Mm(page_h_mm), ops))
    }

    fn text_pages(
        &self,
        _graph: &DocumentGraph,
        unit: &Unit,
        text: &str,
        font: &PdfFontHandle,
        measure: &impl Fn(char) -> f32,
    ) -> Vec<PdfPage> {
        let page_w = 210.0f32;
        let page_h = 297.0f32;
        let margin = 15.0f32;
        let font_pt = 10.0f32;
        let line_mm = 4.5f32;
        let rows = ((page_h - margin * 2.0) / line_mm) as usize;
        let wrapped = wrap_text(
            text,
            (page_w - margin * 2.0) / 25.4 * 72.0 / font_pt,
            measure,
        );
        let mut pages = Vec::new();

        for (page_index, chunk) in wrapped.chunks(rows.max(1)).enumerate() {
            let first_page = page_index == 0;
            let mut ops = vec![
                Op::StartTextSection,
                Op::SetTextRenderingMode {
                    mode: TextRenderingMode::Fill,
                },
                Op::SetFont {
                    font: font.clone(),
                    size: Pt(font_pt),
                },
                Op::SetLineHeight { lh: Pt(13.0) },
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

            let mut hidden_parts = Vec::new();
            if first_page {
                hidden_parts.extend(
                    unit.annotations
                        .iter()
                        .filter(|a| is_searchable_content(&a.kind))
                        .map(annotation_line),
                );
                if let Some(time) = unit.time_range {
                    hidden_parts.push(time_line(time));
                }
            }
            ops.extend(search_layer(&hidden_parts, page_w, page_h, font));

            pages.push(PdfPage::new(Mm(page_w), Mm(page_h), ops));
        }
        pages
    }
}

fn hidden_text_ops(pos: Point, font: PdfFontHandle, size: Pt, text: String) -> Vec<Op> {
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

fn subset_document_font(bytes: &[u8], graph: &DocumentGraph) -> Result<Vec<u8>> {
    use allsorts::{binary::read::ReadScope, font_data::FontData, subset};
    use std::collections::BTreeSet;

    // ASCII also covers generated labels, annotation kinds, timestamps and numbers.
    let mut characters: BTreeSet<char> = (' '..='~').collect();
    for source in &graph.sources {
        characters.extend(source.path.to_string_lossy().chars());
        for (key, value) in &source.metadata {
            characters.extend(key.chars());
            characters.extend(value.chars());
        }
    }
    for unit in &graph.units {
        if let Some(text) = &unit.visible_text {
            characters.extend(text.chars());
        }
        for annotation in &unit.annotations {
            characters.extend(annotation.text.chars());
            characters.extend(annotation.provider.chars());
        }
    }
    let face = ttf_parser::Face::parse(bytes, 0)?;
    let mut glyphs = BTreeSet::from([0]);
    glyphs.extend(
        characters
            .into_iter()
            .filter_map(|ch| face.glyph_index(ch).map(|id| id.0)),
    );
    let font = ReadScope::new(bytes)
        .read::<FontData<'_>>()
        .map_err(|e| anyhow::anyhow!("read font for subsetting: {e:?}"))?;
    let provider = font
        .table_provider(0)
        .map_err(|e| anyhow::anyhow!("read font tables: {e:?}"))?;
    subset::subset(
        &provider,
        &glyphs.into_iter().collect::<Vec<_>>(),
        &subset::SubsetProfile::Pdf,
        subset::CmapTarget::Unicode,
    )
    .map_err(|e| anyhow::anyhow!("subset document font: {e:?}"))
}

fn is_searchable_content(kind: &AnnotationKind) -> bool {
    use AnnotationKind::*;
    matches!(*kind, Ocr | Caption | Transcript | Object | Barcode)
}

fn annotation_line(a: &Annotation) -> String {
    format!("[{:?}][{}] {}", a.kind, a.provider, a.text)
}

fn time_line(t: TimeRange) -> String {
    format!("[TIME] {:.3}-{:.3}s", t.start_seconds, t.end_seconds)
}

// Wrap first, then space every row strictly downward inside the page so rows
// never overlap, wrap around, or get truncated. Spacing and font shrink when crowded.
// Rows are confined below y = height_mm points (the unit the layout is tested in).
fn search_layer(lines: &[String], width_mm: f32, height_mm: f32, font: &PdfFontHandle) -> Vec<Op> {
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

fn wrap_text(text: &str, width: f32, measure: &impl Fn(char) -> f32) -> Vec<String> {
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

fn find_system_font() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ANYTOPDF_FONT") {
        return Some(path.into());
    }
    [
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn refuses_empty_graph_without_touching_destination() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("existing.pdf");
        fs::write(&output, b"keep").unwrap();
        let renderer = SearchablePdfRenderer {
            dpi: 144.0,
            unicode_font: None,
        };
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        assert!(
            renderer
                .render(&ctx, &DocumentGraph::default(), &output)
                .is_err()
        );
        assert_eq!(fs::read(output).unwrap(), b"keep");
    }

    #[test]
    fn successful_image_decode_does_not_emit_informational_warnings() {
        let dir = tempfile::tempdir().unwrap();
        let visual = dir.path().join("image.png");
        ::image::RgbImage::new(4, 3).save(&visual).unwrap();
        let source = SourceRecord::new(visual.clone());
        let graph = DocumentGraph {
            units: vec![Unit::visual(source.id, visual)],
            sources: vec![source],
            ..Default::default()
        };
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let report = SearchablePdfRenderer {
            dpi: 144.0,
            unicode_font: None,
        }
        .render(&ctx, &graph, &dir.path().join("image.pdf"))
        .unwrap();
        assert_eq!(report.pages, 1);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].starts_with("No Unicode font found"));
    }

    #[test]
    fn font_subset_retains_visible_and_metadata_characters() {
        let bytes = BuiltinFont::Helvetica.get_subset_font().bytes;
        let mut source = SourceRecord::new(PathBuf::from("résumé.txt"));
        source.metadata.insert("label".into(), "café".into());
        let graph = DocumentGraph {
            units: vec![Unit::text(source.id, "Résumé éàç".into())],
            sources: vec![source],
            ..Default::default()
        };
        let subset = subset_document_font(&bytes, &graph).unwrap();
        let original_face = ttf_parser::Face::parse(&bytes, 0).unwrap();
        let subset_face = ttf_parser::Face::parse(&subset, 0).unwrap();
        // The library's compact built-in fixture maps spaces to glyph zero.
        for ch in "Résuméeàçcafé.txtlabelSOURCE".chars() {
            assert!(
                original_face.glyph_index(ch).is_some(),
                "fixture lacks {ch}"
            );
            assert!(subset_face.glyph_index(ch).is_some(), "subset lost {ch}");
        }
        assert!(subset_face.number_of_glyphs() < original_face.number_of_glyphs());
    }

    fn invisible_items(pages: &[PdfPage]) -> Vec<Vec<String>> {
        pages
            .iter()
            .map(|page| {
                let mut hidden = false;
                let mut out = Vec::new();
                for op in &page.ops {
                    match op {
                        Op::SetTextRenderingMode { mode } => {
                            hidden = matches!(mode, TextRenderingMode::Invisible)
                        }
                        Op::ShowText { items } if hidden => {
                            for item in items {
                                if let TextItem::Text(t) = item {
                                    out.push(t.clone());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                out
            })
            .collect()
    }

    fn helvetica() -> PdfFontHandle {
        PdfFontHandle::Builtin(BuiltinFont::Helvetica)
    }

    fn renderer() -> SearchablePdfRenderer {
        SearchablePdfRenderer {
            dpi: 144.0,
            unicode_font: None,
        }
    }

    const NOISE: [&str; 5] = [
        "[SOURCE]",
        "[META]",
        "FileAccessDate",
        "FilePermissions",
        "Directory",
    ];

    #[test]
    fn text_pages_hidden_layer_has_no_source_or_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        let mut source = SourceRecord::new(path.clone());
        source
            .metadata
            .insert("exiftool.System:FileAccessDate".into(), "2026:01:01".into());
        source.metadata.insert(
            "exiftool.System:FilePermissions".into(),
            "-rw-r--r--".into(),
        );
        source.metadata.insert(
            "exiftool.System:Directory".into(),
            dir.path().display().to_string(),
        );
        let mut unit = Unit::text(source.id, "visible body".into());
        unit.annotations.push(Annotation::text(
            AnnotationKind::Transcript,
            "test",
            "spoken words marker",
        ));
        let graph = DocumentGraph {
            units: vec![unit.clone()],
            sources: vec![source],
            ..Default::default()
        };
        let pages = renderer().text_pages(&graph, &unit, "visible body", &helvetica(), &|_| 1.0);
        let hidden: Vec<String> = invisible_items(&pages).into_iter().flatten().collect();
        assert!(hidden.iter().any(|t| t.contains("spoken words marker")));
        let path_str = path.display().to_string();
        let dir_str = dir.path().display().to_string();
        for item in &hidden {
            for noise in NOISE {
                assert!(!item.contains(noise), "hidden item {item:?} has {noise}");
            }
            assert!(!item.contains(&path_str), "hidden item {item:?} has path");
            assert!(!item.contains(&dir_str), "hidden item {item:?} has dir");
        }
    }

    #[test]
    fn visual_page_hidden_layer_keeps_captions_but_not_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let visual = dir.path().join("image.png");
        ::image::RgbImage::new(4, 3).save(&visual).unwrap();
        let mut source = SourceRecord::new(visual.clone());
        source
            .metadata
            .insert("exiftool.GPS:GPSLatitude".into(), "51.5".into());
        let mut unit = Unit::visual(source.id, visual.clone());
        unit.time_range = Some(TimeRange {
            start_seconds: 1.0,
            end_seconds: 2.0,
        });
        unit.annotations.push(Annotation::text(
            AnnotationKind::Caption,
            "test",
            "caption marker",
        ));
        let graph = DocumentGraph {
            units: vec![unit.clone()],
            sources: vec![source],
            ..Default::default()
        };
        let mut warnings = Vec::new();
        let mut doc = PdfDocument::new("t");
        let page = renderer()
            .visual_page(
                &mut doc,
                &graph,
                &unit,
                &visual,
                &helvetica(),
                &mut warnings,
            )
            .unwrap();
        let hidden: Vec<String> = invisible_items(&[page]).into_iter().flatten().collect();
        assert!(hidden.iter().any(|t| t.contains("caption marker")));
        assert!(hidden.iter().any(|t| t.contains("[TIME]")));
        let path_str = visual.display().to_string();
        for item in &hidden {
            for noise in ["[SOURCE]", "[META]", "GPSLatitude"] {
                assert!(!item.contains(noise), "hidden item {item:?} has {noise}");
            }
            assert!(!item.contains(&path_str), "hidden item {item:?} has path");
        }
    }

    #[test]
    fn multi_page_text_unit_emits_annotation_block_once() {
        let text: String = (0..400).map(|i| format!("line {i:03}\n")).collect();
        let source = SourceRecord::new(PathBuf::from("long.txt"));
        let mut unit = Unit::text(source.id, text.clone());
        unit.annotations.push(Annotation::text(
            AnnotationKind::Caption,
            "test",
            "UNIQUE-ANNOTATION-MARKER",
        ));
        let graph = DocumentGraph {
            units: vec![unit.clone()],
            sources: vec![source],
            ..Default::default()
        };
        let pages = renderer().text_pages(&graph, &unit, &text, &helvetica(), &|_| 1.0);
        assert!(pages.len() >= 3, "expected >=3 pages, got {}", pages.len());
        let per_page: Vec<usize> = invisible_items(&pages)
            .iter()
            .map(|items| {
                items
                    .iter()
                    .filter(|t| t.contains("UNIQUE-ANNOTATION-MARKER"))
                    .count()
            })
            .collect();
        assert_eq!(per_page.iter().sum::<usize>(), 1, "per page: {per_page:?}");
        assert_eq!(per_page[0], 1, "marker must be on page 0: {per_page:?}");
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
