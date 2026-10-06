use anyhow::{Result, bail};
use anytopdf_core::*;
use printpdf::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
mod attachments;
mod boxes;
mod fonts;
mod layout;
mod pdfa;
mod pdfa_text;
mod provenance;
pub use attachments::{CHUNKS_FILE, EmbeddedFile, MANIFEST_FILE, embed_files, read_embedded_files};
pub use boxes::{BoxKind, DRAW_BOXES_KEY, box_kinds_value, parse_box_kinds};
use fonts::{find_system_font, subset_document_font};
use layout::{
    TEXT_FONT_PT, TEXT_LINE_PT, TEXT_MARGIN_MM, TEXT_PAGE_H_MM, TEXT_PAGE_W_MM, annotation_line,
    hidden_text_ops, is_searchable_content, search_layer, text_page_chunks, time_line,
};
pub use pdfa::PdfARenderer;
use provenance::provenance_pages;
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
        if let Some(created) = graph
            .metadata
            .get("anytopdf.created")
            .and_then(|v| v.parse::<i64>().ok())
            .and_then(|t| OffsetDateTime::from_unix_timestamp(t).ok())
        {
            doc.metadata.info.creation_date = created;
            doc.metadata.info.modification_date = created;
            doc.metadata.info.metadata_date = created;
        }
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

        let mut unit_pages = BTreeMap::new();
        for unit in &graph.units {
            let start = pages.len();
            if let Some(visual) = &unit.visual_path {
                match self.visual_page(
                    &mut doc,
                    graph,
                    unit,
                    visual,
                    &font_handle,
                    &mut pdf_warnings,
                ) {
                    Ok(mut page) => {
                        let (w, h) = (page.media_box.width.0, page.media_box.height.0);
                        page.ops.extend(boxes::printpdf_ops(
                            graph,
                            unit,
                            (w, h),
                            &font_handle,
                            &measure,
                        ));
                        pages.push(page)
                    }
                    Err(e) => {
                        own_warnings.push(format!("visual page {} failed: {e:#}", visual.display()))
                    }
                }
            } else if let Some(text) = &unit.visible_text {
                pages.extend(self.text_pages(graph, unit, text, &font_handle, &measure));
            }
            if pages.len() > start {
                unit_pages.insert(
                    unit.id,
                    PageRange {
                        first: start + 1,
                        last: pages.len(),
                    },
                );
            }
        }

        if pages.is_empty() {
            anyhow::bail!("no PDF pages generated");
        }
        if graph
            .metadata
            .get("anytopdf.provenance-page")
            .is_none_or(|v| v != "off")
        {
            pages.extend(provenance_pages(graph, &unit_pages, &font_handle, &measure));
        }

        let page_count = pages.len();
        doc.with_pages(pages);
        let mut save_warnings = Vec::new();
        let mut lo = doc.to_lopdf_document(
            &PdfSaveOptions {
                subset_fonts: true,
                ..Default::default()
            },
            &mut save_warnings,
        );
        // printpdf draws random trailer IDs; derive them from the content instead.
        lo.trailer.remove(b"ID");
        let mut unsealed = Vec::new();
        lo.save_to(&mut unsealed)?;
        let digest = Sha256::digest(&unsealed);
        let id = lopdf::Object::string_literal(digest[..16].to_vec());
        lo.trailer.set("ID", vec![id.clone(), id]);
        let mut bytes = Vec::new();
        lo.save_to(&mut bytes)?;

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
            unit_pages,
        })
    }
}

impl SearchablePdfRenderer {
    fn load_font(&self, doc: &mut PdfDocument, bytes: Option<&[u8]>) -> Result<PdfFontHandle> {
        if let Some(bytes) = bytes {
            let mut warnings = Vec::new();
            let font = ParsedFont::from_bytes(bytes, 0, &mut warnings)
                .ok_or_else(|| anyhow::anyhow!("could not parse Unicode font"))?;
            let id = FontId("F0".into());
            doc.resources
                .fonts
                .map
                .insert(id.clone(), PdfFont::new(font));
            return Ok(PdfFontHandle::External(id));
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
        let dpi = layout::unit_dpi(unit, self.dpi);
        let page_w_mm = dimensions.0 as f32 / dpi * 25.4;
        let page_h_mm = dimensions.1 as f32 / dpi * 25.4;

        let image_id = XObjectId(format!("Img{}", doc.resources.xobjects.map.len()));
        doc.resources
            .xobjects
            .map
            .insert(image_id.clone(), XObject::Image(raw));
        let mut ops = vec![Op::UseXobject {
            id: image_id,
            transform: XObjectTransform {
                dpi: Some(dpi),
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
                    is_searchable_content(a)
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
        let (page_w, page_h, margin) = (TEXT_PAGE_W_MM, TEXT_PAGE_H_MM, TEXT_MARGIN_MM);
        let mut pages = Vec::new();

        for (page_index, chunk) in text_page_chunks(text, measure).iter().enumerate() {
            let first_page = page_index == 0;
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

            let mut hidden_parts = Vec::new();
            if first_page {
                hidden_parts.extend(
                    unit.annotations
                        .iter()
                        .filter(|a| is_searchable_content(a))
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::provenance::tests::prov_source;

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
        assert_eq!(report.pages, 2);
        assert_eq!(report.warnings.len(), 1);
        assert!(report.warnings[0].starts_with("No Unicode font found"));
    }

    pub(crate) fn invisible_items(pages: &[PdfPage]) -> Vec<Vec<String>> {
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

    pub(crate) fn helvetica() -> PdfFontHandle {
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
    fn visual_dpi_metadata_sets_the_physical_page_size() {
        let dir = tempfile::tempdir().unwrap();
        let visual = dir.path().join("page.png");
        ::image::GrayImage::new(2550, 3300).save(&visual).unwrap();
        let source = SourceRecord::new(visual.clone());
        let page_pt = |dpi: Option<&str>| {
            let mut unit = Unit::visual(source.id, visual.clone());
            if let Some(dpi) = dpi {
                unit.metadata.insert("visual.dpi".into(), dpi.into());
            }
            let page = renderer()
                .visual_page(
                    &mut PdfDocument::new("t"),
                    &DocumentGraph::default(),
                    &unit,
                    &visual,
                    &helvetica(),
                    &mut Vec::new(),
                )
                .unwrap();
            (
                page.media_box.width.0.round(),
                page.media_box.height.0.round(),
            )
        };
        // 300 dpi US Letter is 8.5 x 11 inches.
        assert_eq!(page_pt(Some("300")), (612.0, 792.0));
        // Missing or invalid values fall back to the renderer default.
        assert_eq!(page_pt(None), (1275.0, 1650.0));
        assert_eq!(page_pt(Some("0")), (1275.0, 1650.0));
        assert_eq!(page_pt(Some("abc")), (1275.0, 1650.0));
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

    fn determinism_graph(dir: &Path, text: &str, created: Option<&str>) -> DocumentGraph {
        let visual = dir.join("image.png");
        ::image::RgbImage::new(4, 3).save(&visual).unwrap();
        let mut source = SourceRecord::new(PathBuf::from("input.txt"));
        source.metadata.insert("label".into(), "fixed".into());
        let mut graph = DocumentGraph {
            units: vec![
                Unit::text(source.id, text.into()),
                Unit::visual(source.id, visual),
            ],
            sources: vec![source],
            ..Default::default()
        };
        if let Some(created) = created {
            graph
                .metadata
                .insert("anytopdf.created".into(), created.into());
        }
        graph
    }

    fn render_bytes(dir: &Path, graph: &DocumentGraph, name: &str) -> (Vec<u8>, usize) {
        let ctx = JobContext {
            workspace: dir.into(),
            quiet: true,
        };
        let out = dir.join(name);
        let report = renderer().render(&ctx, graph, &out).unwrap();
        (fs::read(out).unwrap(), report.pages)
    }

    fn trailer_id(bytes: &[u8]) -> String {
        let doc = lopdf::Document::load_mem(bytes).unwrap();
        format!("{:?}", doc.trailer.get(b"ID"))
    }

    const DET_TEXT: &str = "determinism marker\nsecond line\nthird line";

    #[test]
    fn rendering_the_same_graph_twice_is_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let graph = determinism_graph(dir.path(), DET_TEXT, None);
        let (a, _) = render_bytes(dir.path(), &graph, "a.pdf");
        let (b, _) = render_bytes(dir.path(), &graph, "b.pdf");
        assert!(a == b, "same graph rendered to different bytes");
        let other = determinism_graph(
            dir.path(),
            "determinism markes\nsecond line\nthird line",
            None,
        );
        let (c, _) = render_bytes(dir.path(), &other, "c.pdf");
        assert_ne!(trailer_id(&a), trailer_id(&c), "/ID must depend on content");
    }

    #[test]
    fn info_dates_follow_graph_created_epoch() {
        let dir = tempfile::tempdir().unwrap();
        let graph = determinism_graph(dir.path(), DET_TEXT, Some("1700000000"));
        let (bytes, _) = render_bytes(dir.path(), &graph, "dated.pdf");
        let doc = lopdf::Document::load_mem(&bytes).unwrap();
        let info = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        let info = doc.get_dictionary(info).unwrap();
        for key in [&b"CreationDate"[..], &b"ModDate"[..]] {
            let value = info.get(key).unwrap().as_str().unwrap();
            let value = String::from_utf8_lossy(value).to_string();
            assert!(value.starts_with("D:20231114221320"), "{key:?} = {value}");
        }
    }

    #[test]
    fn deterministic_output_keeps_text_extractable_and_page_count() {
        let dir = tempfile::tempdir().unwrap();
        let graph = determinism_graph(dir.path(), DET_TEXT, None);
        let (bytes, pages) = render_bytes(dir.path(), &graph, "text.pdf");
        let doc = lopdf::Document::load_mem(&bytes).unwrap();
        let numbers: Vec<u32> = doc.get_pages().keys().copied().collect();
        assert_eq!(numbers.len(), pages);
        let text: String = numbers
            .iter()
            .map(|n| doc.extract_text(&[*n]).unwrap_or_default())
            .collect();
        assert!(text.contains("determinism marker"), "extracted: {text:?}");
        let id = doc.trailer.get(b"ID").unwrap().as_array().unwrap();
        let lens: Vec<usize> = id.iter().map(|o| o.as_str().unwrap().len()).collect();
        assert_eq!(lens, [16, 16], "/ID must be a pair of 16-byte digests");
    }

    fn content_graph(dir: &Path) -> (DocumentGraph, Uuid, Uuid) {
        let visual = dir.join("image.png");
        ::image::RgbImage::new(4, 3).save(&visual).unwrap();
        let a = prov_source("notes.txt", 'a', 17, "text/plain");
        let b = prov_source("pic.png", 'b', 99, "image/png");
        let body = (1..=150)
            .map(|i| format!("row{i:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let text = Unit::text(a.id, body);
        let image = Unit::visual(b.id, visual);
        let (tid, iid) = (text.id, image.id);
        let graph = DocumentGraph {
            units: vec![text, image],
            sources: vec![a, b],
            ..Default::default()
        };
        (graph, tid, iid)
    }

    fn page_texts(path: &Path) -> Vec<String> {
        let doc = lopdf::Document::load_mem(&fs::read(path).unwrap()).unwrap();
        doc.get_pages()
            .keys()
            .map(|n| doc.extract_text(&[*n]).unwrap_or_default())
            .collect()
    }

    #[test]
    fn content_comes_first_and_provenance_is_last() {
        let dir = tempfile::tempdir().unwrap();
        let (graph, tid, iid) = content_graph(dir.path());
        let out = dir.path().join("order.pdf");
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let report = renderer().render(&ctx, &graph, &out).unwrap();
        assert_eq!(report.pages, 5);
        assert_eq!(
            report.unit_pages.get(&tid),
            Some(&PageRange { first: 1, last: 3 })
        );
        assert_eq!(
            report.unit_pages.get(&iid),
            Some(&PageRange { first: 4, last: 4 })
        );
        assert_eq!(report.unit_pages.len(), graph.units.len());
        let texts = page_texts(&out);
        assert!(texts[0].contains("row001"), "page 1: {:?}", texts[0]);
        assert!(texts[4].contains("SHA-256:"), "page 5: {:?}", texts[4]);
    }

    #[test]
    fn provenance_page_can_be_turned_off() {
        let dir = tempfile::tempdir().unwrap();
        let (mut graph, tid, iid) = content_graph(dir.path());
        graph
            .metadata
            .insert("anytopdf.provenance-page".into(), "off".into());
        let out = dir.path().join("off.pdf");
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let report = renderer().render(&ctx, &graph, &out).unwrap();
        assert_eq!(
            report.unit_pages.get(&tid),
            Some(&PageRange { first: 1, last: 3 })
        );
        assert_eq!(
            report.unit_pages.get(&iid),
            Some(&PageRange { first: 4, last: 4 })
        );
        assert_eq!(report.pages, 4);
        assert!(page_texts(&out).iter().all(|t| !t.contains("SHA-256:")));
    }
}
