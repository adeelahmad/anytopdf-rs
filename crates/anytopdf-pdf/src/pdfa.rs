//! Tagged PDF/A-3a renderer built on krilla.
//!
//! Page layout mirrors [`crate::SearchablePdfRenderer`]. On top of that every font is
//! embedded, the document carries XMP metadata, an sRGB output intent, a structure tree
//! (one section per unit, figures with alternate text, paragraphs per source line) and
//! bookmarks per source, and the manifest and chunks are PDF/A-3 associated files.
use crate::attachments::{CHUNKS_FILE, MANIFEST_FILE};
use crate::boxes::{BoxKind, RectPt, overlay_boxes};
use crate::fonts::{configured_font, find_fallback_fonts};
use crate::layout::{
    SEARCH_X_MM, TEXT_FONT_PT, TEXT_LINE_PT, TEXT_MARGIN_MM, TEXT_PAGE_H_MM, TEXT_PAGE_W_MM,
    TEXT_ROWS_PER_PAGE, TEXT_WRAP_EMS, annotation_line, is_searchable_content, search_rows,
    time_line, wrap_text,
};
use crate::pdfa_text::{FontSet, has_rtl};
use crate::provenance::provenance_lines;
use anyhow::{Context, Result, anyhow, bail};
use anytopdf_core::*;
use krilla::color::rgb;
use krilla::configure::{Archival, ConfigurationBuilder};
use krilla::destination::XyzDestination;
use krilla::embed::{AssociationKind, EmbeddedFile, MimeType};
use krilla::geom::{Point, Size, Transform};
use krilla::image::Image;
use krilla::metadata::{DateTime, Metadata};
use krilla::num::NormalizedF32;
use krilla::outline::{Outline, OutlineNode};
use krilla::page::PageSettings;
use krilla::paint::{Fill, Stroke};
use krilla::surface::Surface;
use krilla::tagging::{
    Artifact, ArtifactType, ContentTag, Identifier, Node, SpanTag, Tag, TagGroup, TagTree,
};
use krilla::{Document, SerializeSettings};
use std::collections::BTreeMap;
use std::fs;
use std::num::NonZeroU16;
use std::path::{Path, PathBuf};

const PT_PER_MM: f32 = 72.0 / 25.4;
/// The graph does not record a document language, so declare it undetermined.
const LANGUAGE: &str = "und";

/// Renders the graph as a tagged PDF/A-3a document (registered as `pdfa`).
pub struct PdfARenderer {
    pub dpi: f32,
    /// The primary font; `None` uses the bundled DejaVu Sans.
    pub unicode_font: Option<PathBuf>,
    /// Fonts used, in order, for characters `unicode_font` lacks.
    pub fallback_fonts: Vec<PathBuf>,
}

impl Default for PdfARenderer {
    fn default() -> Self {
        Self {
            dpi: 144.0,
            unicode_font: configured_font(),
            fallback_fonts: find_fallback_fonts(),
        }
    }
}

impl Plugin for PdfARenderer {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "pdfa".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "renderer".into(),
            extensions: vec!["pdf".into()],
            mime_types: vec!["application/pdf".into()],
            priority: 90,
        }
    }
}

fn visible_fill() -> Fill {
    Fill {
        paint: rgb::Color::black().into(),
        opacity: NormalizedF32::ONE,
        rule: Default::default(),
    }
}

/// krilla has no text rendering mode 3, so the search layer is a fully transparent
/// fill: it is never painted but stays selectable, searchable and extractable.
fn hidden_fill() -> Fill {
    Fill {
        paint: rgb::Color::black().into(),
        opacity: NormalizedF32::ZERO,
        rule: Default::default(),
    }
}

fn pdf_date(unix: i64) -> Result<DateTime> {
    let t = printpdf::OffsetDateTime::from_unix_timestamp(unix)
        .map_err(|e| anyhow!("anytopdf.created is out of range: {e}"))?;
    let year = u16::try_from(t.year()).context("anytopdf.created year")?;
    Ok(DateTime::new(year)
        .month(t.month() as u8)
        .day(t.day())
        .hour(t.hour())
        .minute(t.minute())
        .second(t.second())
        .utc_offset_hour(0)
        .utc_offset_minute(0))
}

fn created(graph: &DocumentGraph) -> Result<i64> {
    match graph.metadata.get("anytopdf.created") {
        Some(v) => v
            .parse()
            .with_context(|| format!("anytopdf.created must be an integer: {v:?}")),
        // PDF/A needs a document date; the CLI always supplies one.
        None => Ok(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default()),
    }
}

fn load_image(path: &Path) -> Result<Image> {
    let bytes = fs::read(path)?;
    let reader = ::image::ImageReader::new(std::io::Cursor::new(&bytes)).with_guessed_format()?;
    let format = reader.format();
    // PNG and baseline RGB/grey JPEG pass straight through; everything else (TIFF, BMP,
    // CMYK JPEG, ...) is decoded to RGBA so no device-dependent colour space remains.
    let direct = match format {
        Some(::image::ImageFormat::Png) => Image::from_png(bytes.clone().into(), false).ok(),
        Some(::image::ImageFormat::Jpeg) if !is_cmyk_jpeg(&bytes) => {
            Image::from_jpeg(bytes.clone().into(), false).ok()
        }
        _ => None,
    };
    if let Some(image) = direct {
        return Ok(image);
    }
    let decoded = reader.decode().context("decode image")?.into_rgba8();
    let (w, h) = decoded.dimensions();
    Ok(Image::from_rgba8(decoded.into_raw(), w, h))
}

fn is_cmyk_jpeg(bytes: &[u8]) -> bool {
    use ::image::ImageDecoder;
    ::image::codecs::jpeg::JpegDecoder::new(std::io::Cursor::new(bytes))
        .map(|d| d.original_color_type() == ::image::ExtendedColorType::Cmyk8)
        .unwrap_or(true)
}

impl Renderer for PdfARenderer {
    fn render(
        &self,
        _ctx: &JobContext,
        graph: &DocumentGraph,
        output: &Path,
    ) -> Result<RenderReport> {
        if !self.dpi.is_finite() || self.dpi <= 0.0 {
            bail!("PDF DPI must be finite and positive");
        }
        let mut warnings = Vec::new();
        let font = FontSet::load(
            self.unicode_font.as_deref(),
            &self.fallback_fonts,
            graph,
            &mut warnings,
        )?;
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
            .filter(|c| !c.is_control() && !c.is_whitespace() && !font.has(*c))
            .collect();
        if !missing.is_empty() {
            warnings.push(format!(
                "Font lacks {} character(s): {:?}; PDF/A output omits them. Add a font covering this script to ANYTOPDF_FONT.",
                missing.len(),
                missing.iter().take(20).collect::<String>()
            ));
        }

        let configuration = ConfigurationBuilder::new()
            .with_archival_validator(Archival::A3_A)
            .finish()
            .map_err(|e| anyhow!("PDF/A-3a configuration: {e:?}"))?;
        let settings = SerializeSettings {
            configuration,
            enable_tagging: true,
            pretty: false,
            ..Default::default()
        };
        let date = pdf_date(created(graph)?)?;
        let mut builder = Builder {
            doc: Document::new_with(settings),
            font: &font,
            box_kinds: crate::boxes::requested_kinds(graph),
            pages: 0,
            tree: TagTree::new().with_lang(Some(LANGUAGE.into())),
        };
        builder.doc.set_metadata(
            Metadata::new()
                .title("anytopdf".into())
                .language(LANGUAGE.into())
                .creator("anytopdf".into())
                .producer(format!("anytopdf {}", env!("CARGO_PKG_VERSION")))
                .creation_date(date),
        );

        let mut unit_pages = BTreeMap::new();
        for unit in &graph.units {
            let start = builder.pages;
            let mut section = TagGroup::new(Tag::Section);
            if let Some(visual) = &unit.visual_path {
                match load_image(visual) {
                    Ok(image) => builder.visual_page(
                        &mut section,
                        unit,
                        image,
                        crate::layout::unit_dpi(unit, self.dpi),
                    ),
                    Err(e) => {
                        warnings.push(format!("visual page {} failed: {e:#}", visual.display()))
                    }
                }
            } else if let Some(text) = &unit.visible_text {
                builder.text_unit(&mut section, unit, text);
            }
            if builder.pages > start {
                builder.tree.push(section);
                unit_pages.insert(
                    unit.id,
                    PageRange {
                        first: start + 1,
                        last: builder.pages,
                    },
                );
            }
        }
        if builder.pages == 0 {
            bail!("no PDF pages generated");
        }
        let mut outline = source_outline(graph, &unit_pages);
        if graph
            .metadata
            .get("anytopdf.provenance-page")
            .is_none_or(|v| v != "off")
        {
            let first = builder.pages;
            builder.provenance(&provenance_lines(graph, &unit_pages));
            outline.push_child(OutlineNode::new(
                "Provenance".into(),
                XyzDestination::new(first, Point::from_xy(0.0, 0.0)),
            ));
        }
        let Builder {
            mut doc,
            pages,
            tree,
            ..
        } = builder;
        doc.set_outline(outline);
        doc.set_tag_tree(tree);

        let report = RenderReport {
            pages,
            warnings,
            unit_pages,
        };
        let manifest = serde_json::to_vec_pretty(&Manifest::build(graph, &report))?;
        let chunks = serde_json::to_vec_pretty(&ChunkSet::build(graph, &report))?;
        for (name, description, data) in [
            (CHUNKS_FILE, "anytopdf chunks (anytopdf.chunks/1)", chunks),
            (
                MANIFEST_FILE,
                "anytopdf manifest (anytopdf.manifest/1)",
                manifest,
            ),
        ] {
            doc.embed_file(EmbeddedFile {
                path: name.into(),
                mime_type: MimeType::new("application/json"),
                description: Some(description.into()),
                association_kind: AssociationKind::Data,
                data: data.into(),
                modification_date: Some(date),
                compress: Some(true),
                location: None,
            })
            .ok_or_else(|| anyhow!("duplicate embedded file {name}"))?;
        }

        let bytes = doc
            .finish()
            .map_err(|e| anyhow!("PDF/A-3a export failed: {e:?}"))?;
        atomic_write(output, &bytes)?;
        Ok(report)
    }
}

/// One bookmark per source, pointing at the first page rendered from it.
fn source_outline(graph: &DocumentGraph, unit_pages: &BTreeMap<Uuid, PageRange>) -> Outline {
    let mut outline = Outline::new();
    for source in &graph.sources {
        let first = graph
            .units
            .iter()
            .filter(|u| u.source_id == source.id)
            .filter_map(|u| unit_pages.get(&u.id))
            .map(|r| r.first)
            .min();
        if let Some(first) = first {
            outline.push_child(OutlineNode::new(
                basename(&source.path),
                XyzDestination::new(first - 1, Point::from_xy(0.0, 0.0)),
            ));
        }
    }
    outline
}

/// Horizontal scale that fits a word of `natural` width into an OCR box `box_w`
/// wide, bounded so a degenerate box cannot collapse or smear the text.
fn ocr_stretch(box_w: f32, natural: f32) -> f32 {
    if box_w > 0.0 && natural > 0.0 {
        (box_w / natural).clamp(0.25, 4.0)
    } else {
        1.0
    }
}

/// Draw `text` as one tagged span and return its identifier, or `None` for blank text.
fn span(
    surface: &mut Surface,
    font: &FontSet,
    at: Point,
    size: f32,
    text: &str,
) -> Option<Identifier> {
    if text.trim().is_empty() {
        return None;
    }
    // Right-to-left text is drawn in visual order; ActualText keeps the logical order
    // for copy and extraction.
    let actual = has_rtl(text).then_some(text);
    let id = surface.start_tagged(ContentTag::Span(SpanTag::empty().with_actual_text(actual)));
    font.draw(surface, at, size, text);
    surface.end_tagged();
    Some(id)
}

fn paragraph(ids: impl IntoIterator<Item = Identifier>) -> TagGroup {
    TagGroup::with_children(Tag::P, ids.into_iter().map(Node::from).collect())
}

fn new_page(doc: &mut Document, w: f32, h: f32) -> krilla::page::Page<'_> {
    let size = Size::from_wh(w, h).expect("page size is finite and positive");
    doc.start_page_with(PageSettings::new(size))
}

/// Accumulates pages, the structure tree and the page count while rendering.
struct Builder<'a> {
    doc: Document,
    font: &'a FontSet,
    box_kinds: Vec<BoxKind>,
    pages: usize,
    tree: TagTree,
}

impl Builder<'_> {
    fn visual_page(&mut self, section: &mut TagGroup, unit: &Unit, image: Image, dpi: f32) {
        let font = self.font;
        let (w_px, h_px) = image.size();
        let page_w_mm = w_px as f32 / dpi * 25.4;
        let page_h_mm = h_px as f32 / dpi * 25.4;
        let (w, h) = (page_w_mm * PT_PER_MM, page_h_mm * PT_PER_MM);
        let mut page = new_page(&mut self.doc, w, h);
        let mut surface = page.surface();
        let figure_id = surface.start_tagged(ContentTag::Other);
        if let Some(size) = Size::from_wh(w, h) {
            surface.draw_image(image, size);
        }
        surface.end_tagged();
        draw_boxes(&mut surface, font, &self.box_kinds, unit, w, h);

        surface.set_fill(Some(hidden_fill()));
        // Positioned OCR layer, placed exactly as the printpdf renderer places it.
        let mut ocr = Vec::new();
        for annotation in unit
            .annotations
            .iter()
            .filter(|a| a.kind == AnnotationKind::Ocr)
        {
            if let Some(region) = annotation.region {
                let r = region.clamped();
                let x = page_w_mm * r.x;
                let y_top = page_h_mm * r.y;
                let box_h = (page_h_mm * r.height).max(1.0);
                let baseline_y = (page_h_mm - y_top - box_h * 0.85).max(0.5);
                let font_pt = ((box_h / 25.4) * 72.0 * 0.78).clamp(3.0, 72.0);
                let at = Point::from_xy(x * PT_PER_MM, (page_h_mm - baseline_y) * PT_PER_MM);
                // Stretch the word to its OCR box so selection and search highlights
                // cover the word in the image, not the bundled font's natural width.
                let box_w = page_w_mm * r.width * PT_PER_MM;
                let stretch = ocr_stretch(box_w, font.width(&annotation.text) * font_pt);
                surface.push_transform(&Transform::from_row(
                    stretch,
                    0.0,
                    0.0,
                    1.0,
                    at.x * (1.0 - stretch),
                    0.0,
                ));
                ocr.extend(span(&mut surface, font, at, font_pt, &annotation.text));
                surface.pop();
            }
        }

        let mut lines = Vec::new();
        if let Some(t) = unit.time_range {
            lines.push(time_line(t));
        }
        lines.extend(
            unit.annotations
                .iter()
                .filter(|a| {
                    is_searchable_content(&a.kind)
                        && (a.kind != AnnotationKind::Ocr || a.region.is_none())
                })
                .map(annotation_line),
        );
        let hidden = search_layer(&mut surface, font, &lines, page_w_mm, page_h_mm);
        surface.finish();
        page.finish();
        self.pages += 1;

        let alt = match unit.time_range {
            Some(t) => format!(
                "Video frame at {:.3} to {:.3} seconds",
                t.start_seconds, t.end_seconds
            ),
            None => "Image".into(),
        };
        section.push(TagGroup::with_children(
            Tag::Figure(Some(alt)),
            vec![figure_id.into()],
        ));
        if !ocr.is_empty() {
            section.push(paragraph(ocr));
        }
        for id in hidden {
            section.push(paragraph([id]));
        }
    }

    fn text_unit(&mut self, section: &mut TagGroup, unit: &Unit, text: &str) {
        // Wrap per source line so each line becomes one paragraph; the rows are the
        // same ones `text_page_chunks` produces for the printpdf renderer.
        let font = self.font;
        let mut rows: Vec<(usize, String)> = text
            .lines()
            .enumerate()
            .flat_map(|(i, line)| {
                wrap_text(line, TEXT_WRAP_EMS, &|c| font.measure(c))
                    .into_iter()
                    .map(move |row| (i, row))
            })
            .collect();
        if rows.is_empty() {
            rows.push((0, String::new()));
        }
        let mut paragraphs: BTreeMap<usize, Vec<Identifier>> = BTreeMap::new();
        let mut hidden_ids = Vec::new();
        for (index, chunk) in rows.chunks(TEXT_ROWS_PER_PAGE).enumerate() {
            let mut hidden = Vec::new();
            if index == 0 {
                hidden.extend(
                    unit.annotations
                        .iter()
                        .filter(|a| is_searchable_content(&a.kind))
                        .map(annotation_line),
                );
                if let Some(time) = unit.time_range {
                    hidden.push(time_line(time));
                }
            }
            let lines: Vec<&str> = chunk.iter().map(|(_, row)| row.as_str()).collect();
            let (ids, more) = self.text_page(&lines, &hidden);
            for ((para, _), id) in chunk.iter().zip(ids) {
                paragraphs.entry(*para).or_default().extend(id);
            }
            hidden_ids.extend(more);
        }
        for ids in paragraphs.into_values() {
            section.push(paragraph(ids));
        }
        for id in hidden_ids {
            section.push(paragraph([id]));
        }
    }

    fn provenance(&mut self, lines: &[String]) {
        let font = self.font;
        let text = lines.join("\n");
        let rows = wrap_text(&text, TEXT_WRAP_EMS, &|c| font.measure(c));
        let mut section = TagGroup::new(Tag::Section);
        let mut heading = true;
        for chunk in rows.chunks(TEXT_ROWS_PER_PAGE) {
            let lines: Vec<&str> = chunk.iter().map(String::as_str).collect();
            let (ids, _) = self.text_page(&lines, &[]);
            for id in ids.into_iter().flatten() {
                if std::mem::take(&mut heading) {
                    let level = NonZeroU16::MIN;
                    section.push(TagGroup::with_children(
                        Tag::Hn(level, Some("Provenance".into())),
                        vec![id.into()],
                    ));
                } else {
                    section.push(paragraph([id]));
                }
            }
        }
        self.tree.push(section);
    }

    /// Draw one visible text page; returns one identifier slot per row and the
    /// identifiers of the hidden search rows.
    fn text_page(
        &mut self,
        lines: &[&str],
        hidden: &[String],
    ) -> (Vec<Option<Identifier>>, Vec<Identifier>) {
        let font = self.font;
        let mut page = new_page(
            &mut self.doc,
            TEXT_PAGE_W_MM * PT_PER_MM,
            TEXT_PAGE_H_MM * PT_PER_MM,
        );
        let mut surface = page.surface();
        surface.set_fill(Some(visible_fill()));
        let left = TEXT_MARGIN_MM * PT_PER_MM;
        let top = TEXT_MARGIN_MM * PT_PER_MM;
        let ids = lines
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let at = Point::from_xy(left, top + TEXT_LINE_PT * i as f32);
                span(&mut surface, font, at, TEXT_FONT_PT, line)
            })
            .collect();
        let hidden = search_layer(&mut surface, font, hidden, TEXT_PAGE_W_MM, TEXT_PAGE_H_MM);
        surface.finish();
        page.finish();
        self.pages += 1;
        (ids, hidden)
    }
}

/// Vector overlay for `--draw-boxes`. It is decoration over the figure, so it is
/// tagged as an artifact; the annotations it shows are already in the search layer.
fn draw_boxes(
    surface: &mut Surface,
    font: &FontSet,
    kinds: &[BoxKind],
    unit: &Unit,
    w: f32,
    h: f32,
) {
    let overlay = overlay_boxes(kinds, unit, w, h, &|text| font.width(text));
    if overlay.is_empty() {
        return;
    }
    let solid = |kind: BoxKind| {
        let (r, g, b) = kind.rgb();
        rgb::Color::new(r, g, b)
    };
    let rect_path = |r: RectPt| {
        let mut path = krilla::geom::PathBuilder::new();
        path.push_rect(krilla::geom::Rect::from_xywh(r.x, r.y, r.w, r.h)?);
        path.finish()
    };
    surface.start_tagged(ContentTag::Artifact(Artifact::with_kind(
        ArtifactType::Other,
    )));
    for b in overlay {
        if let Some(path) = rect_path(b.rect) {
            surface.set_fill(None);
            surface.set_stroke(Some(Stroke {
                paint: solid(b.kind).into(),
                width: b.stroke,
                ..Default::default()
            }));
            surface.draw_path(&path);
        }
        surface.set_stroke(None);
        if let Some(label) = b.label {
            if let Some(path) = rect_path(label.rect) {
                surface.set_fill(Some(Fill {
                    paint: solid(b.kind).into(),
                    opacity: NormalizedF32::ONE,
                    rule: Default::default(),
                }));
                surface.draw_path(&path);
            }
            surface.set_fill(Some(Fill {
                paint: rgb::Color::white().into(),
                opacity: NormalizedF32::ONE,
                rule: Default::default(),
            }));
            font.draw(
                surface,
                Point::from_xy(label.baseline_x, label.baseline_y),
                label.size,
                &label.text,
            );
        }
    }
    surface.end_tagged();
}

fn search_layer(
    surface: &mut Surface,
    font: &FontSet,
    lines: &[String],
    width_mm: f32,
    height_mm: f32,
) -> Vec<Identifier> {
    surface.set_fill(Some(hidden_fill()));
    search_rows(lines, width_mm, height_mm)
        .into_iter()
        .filter_map(|(y_mm, size, row)| {
            let at = Point::from_xy(SEARCH_X_MM * PT_PER_MM, (height_mm - y_mm) * PT_PER_MM);
            span(surface, font, at, size, &row)
        })
        .collect()
}

#[cfg(test)]
#[path = "pdfa_tests.rs"]
mod tests;
