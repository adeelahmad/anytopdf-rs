//! PDF/A-3b renderer built on krilla.
//!
//! Page layout mirrors [`crate::SearchablePdfRenderer`]; the differences are that every
//! font is embedded, the document carries XMP metadata and an sRGB output intent, and the
//! manifest and chunks are written as PDF/A-3 associated files instead of being added
//! after rendering.
use crate::attachments::{CHUNKS_FILE, MANIFEST_FILE};
use crate::fonts::{find_system_font, subset_document_font};
use crate::layout::{
    SEARCH_X_MM, TEXT_FONT_PT, TEXT_LINE_PT, TEXT_MARGIN_MM, TEXT_PAGE_H_MM, TEXT_PAGE_W_MM,
    annotation_line, is_searchable_content, search_rows, text_page_chunks, time_line,
};
use crate::provenance::provenance_lines;
use anyhow::{Context, Result, anyhow, bail};
use anytopdf_core::*;
use krilla::color::rgb;
use krilla::configure::{Configuration, Validator};
use krilla::embed::{AssociationKind, EmbeddedFile, MimeType};
use krilla::geom::{Point, Size};
use krilla::image::Image;
use krilla::metadata::{DateTime, Metadata};
use krilla::num::NormalizedF32;
use krilla::page::PageSettings;
use krilla::paint::Fill;
use krilla::surface::Surface;
use krilla::text::{Font, TextDirection};
use krilla::{Document, SerializeSettings};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

const PT_PER_MM: f32 = 72.0 / 25.4;

/// Renders the graph as a PDF/A-3b document (registered as `pdfa`).
pub struct PdfARenderer {
    pub dpi: f32,
    pub unicode_font: Option<PathBuf>,
}

impl Default for PdfARenderer {
    fn default() -> Self {
        Self {
            dpi: 144.0,
            unicode_font: find_system_font(),
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

/// The embedded font plus the per-character facts layout needs.
struct TextFont {
    font: Font,
    advances: BTreeMap<char, f32>,
}

impl TextFont {
    fn load(bytes: Vec<u8>, graph: &DocumentGraph) -> Result<Self> {
        let subset = subset_document_font(&bytes, graph)?;
        let face = ttf_parser::Face::parse(&subset, 0).context("parse subset font")?;
        let em = f32::from(face.units_per_em());
        let mut advances = BTreeMap::new();
        for table in face.tables().cmap.iter().flat_map(|c| c.subtables) {
            if !table.is_unicode() {
                continue;
            }
            table.codepoints(|cp| {
                if let Some(ch) = char::from_u32(cp)
                    && let Some(glyph) = face.glyph_index(ch).filter(|g| g.0 != 0)
                {
                    let advance = face.glyph_hor_advance(glyph).unwrap_or(0);
                    advances.insert(ch, f32::from(advance) / em);
                }
            });
        }
        let font = Font::new(subset.into(), 0).ok_or_else(|| anyhow!("could not load font"))?;
        Ok(Self { font, advances })
    }

    fn has(&self, ch: char) -> bool {
        self.advances.contains_key(&ch)
    }

    fn measure(&self, ch: char) -> f32 {
        self.advances.get(&ch).copied().unwrap_or(0.5)
    }

    /// Draw `text` with its baseline at `origin` (points, y down). Characters the font
    /// cannot show are skipped (PDF/A forbids `.notdef`) but still advance the pen so
    /// the surrounding runs keep their positions.
    fn draw(&self, surface: &mut Surface, origin: Point, size: f32, text: &str) {
        let mut x = origin.x;
        let mut run = String::new();
        let mut run_x = x;
        for ch in text.chars().filter(|c| !c.is_control()) {
            if self.has(ch) {
                if run.is_empty() {
                    run_x = x;
                }
                run.push(ch);
            } else {
                self.flush(surface, &mut run, Point::from_xy(run_x, origin.y), size);
            }
            x += self.measure(ch) * size;
        }
        self.flush(surface, &mut run, Point::from_xy(run_x, origin.y), size);
    }

    fn flush(&self, surface: &mut Surface, run: &mut String, at: Point, size: f32) {
        if !run.is_empty() {
            surface.draw_text(at, self.font.clone(), size, run, false, TextDirection::Auto);
            run.clear();
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
        Some(::image::ImageFormat::Png) => Image::from_png(bytes.clone().into(), false),
        Some(::image::ImageFormat::Jpeg) if !is_cmyk_jpeg(&bytes) => {
            Image::from_jpeg(bytes.clone().into(), false)
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
        let font_path = self.unicode_font.as_ref().ok_or_else(|| {
            anyhow!("PDF/A output must embed its font; set ANYTOPDF_FONT to a TTF font")
        })?;
        let font_bytes =
            fs::read(font_path).with_context(|| format!("read font {}", font_path.display()))?;
        let font = TextFont::load(font_bytes, graph)?;
        let mut warnings = Vec::new();
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
                "Font lacks {} character(s): {:?}; PDF/A output omits them. Set ANYTOPDF_FONT to a font covering this script.",
                missing.len(),
                missing.iter().take(20).collect::<String>()
            ));
        }

        let created = created(graph)?;
        let settings = SerializeSettings {
            configuration: Configuration::new_with_validator(Validator::A3_B),
            enable_tagging: false,
            ..Default::default()
        };
        let mut doc = Document::new_with(settings);
        let date = pdf_date(created)?;
        doc.set_metadata(
            Metadata::new()
                .title("anytopdf".into())
                .creator("anytopdf".into())
                .producer(format!("anytopdf {}", env!("CARGO_PKG_VERSION")))
                .creation_date(date),
        );

        let mut pages = 0usize;
        let mut unit_pages = BTreeMap::new();
        for unit in &graph.units {
            let start = pages;
            if let Some(visual) = &unit.visual_path {
                match load_image(visual) {
                    Ok(image) => {
                        self.visual_page(&mut doc, &font, unit, image);
                        pages += 1;
                    }
                    Err(e) => {
                        warnings.push(format!("visual page {} failed: {e:#}", visual.display()))
                    }
                }
            } else if let Some(text) = &unit.visible_text {
                pages += text_pages(&mut doc, &font, unit, text);
            }
            if pages > start {
                unit_pages.insert(
                    unit.id,
                    PageRange {
                        first: start + 1,
                        last: pages,
                    },
                );
            }
        }
        if pages == 0 {
            bail!("no PDF pages generated");
        }
        if graph
            .metadata
            .get("anytopdf.provenance-page")
            .is_none_or(|v| v != "off")
        {
            let text = provenance_lines(graph, &unit_pages).join("\n");
            for chunk in text_page_chunks(&text, &|c| font.measure(c)) {
                visible_text_page(&mut doc, &font, &chunk, &[]);
                pages += 1;
            }
        }

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
            .map_err(|e| anyhow!("PDF/A-3b export failed: {e:?}"))?;
        atomic_write(output, &bytes)?;
        Ok(report)
    }
}

impl PdfARenderer {
    fn visual_page(&self, doc: &mut Document, font: &TextFont, unit: &Unit, image: Image) {
        let (w_px, h_px) = image.size();
        let dpi = crate::layout::unit_dpi(unit, self.dpi);
        let page_w_mm = w_px as f32 / dpi * 25.4;
        let page_h_mm = h_px as f32 / dpi * 25.4;
        let (w, h) = (page_w_mm * PT_PER_MM, page_h_mm * PT_PER_MM);
        let mut page = doc.start_page_with(PageSettings::new(w, h));
        let mut surface = page.surface();
        if let Some(size) = Size::from_wh(w, h) {
            surface.draw_image(image, size);
        }

        surface.set_fill(Some(hidden_fill()));
        // Positioned OCR layer, placed exactly as the printpdf renderer places it.
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
                font.draw(
                    &mut surface,
                    Point::from_xy(x * PT_PER_MM, (page_h_mm - baseline_y) * PT_PER_MM),
                    font_pt,
                    &annotation.text,
                );
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
        draw_search_layer(&mut surface, font, &lines, page_w_mm, page_h_mm);
        surface.finish();
        page.finish();
    }
}

fn draw_search_layer(
    surface: &mut Surface,
    font: &TextFont,
    lines: &[String],
    width_mm: f32,
    height_mm: f32,
) {
    surface.set_fill(Some(hidden_fill()));
    for (y_mm, size, row) in search_rows(lines, width_mm, height_mm) {
        font.draw(
            surface,
            Point::from_xy(SEARCH_X_MM * PT_PER_MM, (height_mm - y_mm) * PT_PER_MM),
            size,
            &row,
        );
    }
}

fn text_pages(doc: &mut Document, font: &TextFont, unit: &Unit, text: &str) -> usize {
    let chunks = text_page_chunks(text, &|c| font.measure(c));
    for (index, chunk) in chunks.iter().enumerate() {
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
        visible_text_page(doc, font, chunk, &hidden);
    }
    chunks.len()
}

fn visible_text_page(doc: &mut Document, font: &TextFont, lines: &[String], hidden: &[String]) {
    let mut page = doc.start_page_with(PageSettings::new(
        TEXT_PAGE_W_MM * PT_PER_MM,
        TEXT_PAGE_H_MM * PT_PER_MM,
    ));
    let mut surface = page.surface();
    surface.set_fill(Some(visible_fill()));
    let left = TEXT_MARGIN_MM * PT_PER_MM;
    let top = TEXT_MARGIN_MM * PT_PER_MM;
    for (i, line) in lines.iter().enumerate() {
        let y = top + TEXT_LINE_PT * i as f32;
        font.draw(&mut surface, Point::from_xy(left, y), TEXT_FONT_PT, line);
    }
    draw_search_layer(&mut surface, font, hidden, TEXT_PAGE_W_MM, TEXT_PAGE_H_MM);
    surface.finish();
    page.finish();
}

#[cfg(test)]
mod tests {
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
    fn declares_pdfa3b_with_output_intent_and_xmp() {
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
            xmp.contains("<pdfaid:conformance>B</pdfaid:conformance>"),
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
        let (resources, _) = doc.get_page_resources(page).unwrap();
        let states = resources.unwrap().get(b"ExtGState").unwrap();
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
    fn missing_font_fails_without_touching_destination() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("existing.pdf");
        fs::write(&output, b"keep").unwrap();
        let err = PdfARenderer {
            dpi: 144.0,
            unicode_font: None,
        }
        .render(&ctx(dir.path()), &graph(dir.path()), &output)
        .unwrap_err();
        assert!(err.to_string().contains("ANYTOPDF_FONT"), "{err:#}");
        assert_eq!(fs::read(output).unwrap(), b"keep");
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
}
