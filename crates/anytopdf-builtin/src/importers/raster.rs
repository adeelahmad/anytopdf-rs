//! PWG Raster (PWG 5102.4) and Apple Raster (URF) print-job importer.
//!
//! IPP Everywhere and AirPrint clients send print jobs in these formats. Each
//! page becomes one visual unit carrying its print resolution in
//! `visual.dpi`, so the renderer reproduces the physical page size.

use anyhow::{Context, Result, bail, ensure};
use anytopdf_core::{
    Anchor, Diagnostic, DiagnosticCode, ImportOutcome, Importer, JobContext, Plugin,
    PluginDescriptor, ProbeScore, SourceRecord, Unit,
};
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read};
use std::path::Path;

const PWG_SYNC: &[u8; 4] = b"RaS2";
const URF_SYNC: &[u8; 8] = b"UNIRAST\0";
const PWG_HEADER_LEN: usize = 1796;
const URF_HEADER_LEN: usize = 32;
/// Largest page accepted, in pixels (about 600 dpi on A3 paper, with room).
const MAX_PAGE_PIXELS: u64 = 1 << 27;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Pwg,
    Urf,
}

impl Flavor {
    fn sniff(path: &Path) -> Option<Self> {
        let mut magic = Vec::with_capacity(8);
        File::open(path)
            .ok()?
            .take(8)
            .read_to_end(&mut magic)
            .ok()?;
        if magic == URF_SYNC {
            Some(Self::Urf)
        } else if magic.starts_with(PWG_SYNC) {
            Some(Self::Pwg)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Pwg => "pwg-raster",
            Self::Urf => "apple-raster",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Color {
    /// Additive grey: 0 is black.
    Gray,
    /// Subtractive black: 0 is white.
    Black,
    Rgb,
    Cmyk,
}

impl Color {
    fn channels(self) -> usize {
        match self {
            Self::Gray | Self::Black => 1,
            Self::Rgb => 3,
            Self::Cmyk => 4,
        }
    }

    /// Byte value that paints white in this colour space.
    fn white(self) -> u8 {
        match self {
            Self::Gray | Self::Rgb => 0xff,
            Self::Black | Self::Cmyk => 0x00,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PageHeader {
    width: u32,
    height: u32,
    dpi: (u32, u32),
    bits_per_color: u32,
    color: Color,
}

impl PageHeader {
    fn bits_per_pixel(&self) -> usize {
        self.bits_per_color as usize * self.color.channels()
    }

    /// Bytes in one compression unit: a whole pixel, or one byte when pixels
    /// are smaller than a byte.
    fn unit_bytes(&self) -> usize {
        self.bits_per_pixel().div_ceil(8)
    }

    fn line_bytes(&self) -> usize {
        (self.width as usize * self.bits_per_pixel()).div_ceil(8)
    }

    fn validate(&self) -> Result<()> {
        ensure!(
            self.width > 0 && self.height > 0,
            "raster page has no pixels"
        );
        ensure!(
            u64::from(self.width) * u64::from(self.height) <= MAX_PAGE_PIXELS,
            "raster page {}x{} exceeds the {MAX_PAGE_PIXELS}-pixel limit",
            self.width,
            self.height
        );
        ensure!(
            self.dpi.0 > 0 && self.dpi.1 > 0,
            "raster page has no resolution"
        );
        let supported = match self.bits_per_color {
            1 => matches!(self.color, Color::Gray | Color::Black),
            8 | 16 => true,
            _ => false,
        };
        ensure!(
            supported,
            "unsupported raster depth: {} bits per colour for {:?}",
            self.bits_per_color,
            self.color
        );
        Ok(())
    }
}

fn be_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes"))
}

fn parse_pwg_header(h: &[u8; PWG_HEADER_LEN]) -> Result<PageHeader> {
    let color = match be_u32(h, 400) {
        0 | 18 => Color::Gray,     // W, sGray
        3 => Color::Black,         // K
        1 | 19 | 20 => Color::Rgb, // RGB, sRGB, AdobeRGB
        6 => Color::Cmyk,
        other => bail!("unsupported PWG raster colour space {other}"),
    };
    ensure!(
        be_u32(h, 396) == 0,
        "only chunky PWG raster pages are supported"
    );
    let header = PageHeader {
        width: be_u32(h, 372),
        height: be_u32(h, 376),
        dpi: (be_u32(h, 276), be_u32(h, 280)),
        bits_per_color: be_u32(h, 384),
        color,
    };
    ensure!(
        be_u32(h, 388) as usize == header.bits_per_pixel(),
        "PWG raster bits per pixel do not match the colour space"
    );
    ensure!(
        be_u32(h, 392) as usize == header.line_bytes(),
        "PWG raster bytes per line do not match the page width"
    );
    header.validate()?;
    Ok(header)
}

fn parse_urf_header(h: &[u8; URF_HEADER_LEN]) -> Result<PageHeader> {
    let bpp = u32::from(h[0]);
    let color = match h[1] {
        0 | 4 => Color::Gray,    // sGray, DeviceGray
        1 | 3 | 5 => Color::Rgb, // sRGB, AdobeRGB, DeviceRGB
        6 => Color::Cmyk,
        other => bail!("unsupported Apple raster colour space {other}"),
    };
    ensure!(
        bpp % color.channels() as u32 == 0,
        "Apple raster bits per pixel do not match the colour space"
    );
    let dpi = be_u32(h, 20);
    let header = PageHeader {
        width: be_u32(h, 12),
        height: be_u32(h, 16),
        dpi: (dpi, dpi),
        bits_per_color: bpp / color.channels() as u32,
        color,
    };
    header.validate()?;
    Ok(header)
}

/// Reads exactly `buf.len()` bytes, returning `false` on a clean end of file
/// before the first byte.
fn read_or_eof(reader: &mut impl Read, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => bail!("raster data ends inside a page header"),
            Ok(n) => filled += n,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(true)
}

fn read_byte(reader: &mut impl Read) -> Result<u8> {
    let mut b = [0u8; 1];
    reader
        .read_exact(&mut b)
        .context("raster data ends inside a page")?;
    Ok(b[0])
}

/// Decodes one page of PackBits-style raster data shared by PWG and Apple
/// raster: a line-repeat byte, then runs of repeated or literal pixels, with
/// 0x80 filling the rest of the line with white.
fn decode_page(reader: &mut impl Read, header: &PageHeader) -> Result<Vec<u8>> {
    let line_bytes = header.line_bytes();
    let unit = header.unit_bytes();
    let white = header.color.white();
    let height = header.height as usize;
    // Grow with the data actually present so a lying header cannot force a
    // large allocation up front.
    let mut page = Vec::with_capacity((line_bytes * height).min(1 << 24));
    let mut line = vec![0u8; line_bytes];
    let mut pixel = vec![0u8; unit];
    while page.len() < line_bytes * height {
        let repeat = usize::from(read_byte(reader)?) + 1;
        let mut x = 0;
        while x < line_bytes {
            let control = read_byte(reader)?;
            match control {
                0x80 => {
                    line[x..].fill(white);
                    x = line_bytes;
                }
                0..=0x7f => {
                    reader
                        .read_exact(&mut pixel)
                        .context("raster data ends inside a page")?;
                    for _ in 0..=control {
                        let n = unit.min(line_bytes - x);
                        ensure!(n > 0, "raster run overflows its line");
                        line[x..x + n].copy_from_slice(&pixel[..n]);
                        x += n;
                    }
                }
                _ => {
                    let bytes = (257 - usize::from(control)) * unit;
                    ensure!(x + bytes <= line_bytes, "raster run overflows its line");
                    reader
                        .read_exact(&mut line[x..x + bytes])
                        .context("raster data ends inside a page")?;
                    x += bytes;
                }
            }
        }
        for _ in 0..repeat.min(height - page.len() / line_bytes) {
            page.extend_from_slice(&line);
        }
    }
    Ok(page)
}

fn to_image(header: &PageHeader, data: Vec<u8>) -> Result<image::DynamicImage> {
    let (w, h) = (header.width, header.height);
    let channels = header.color.channels();
    let samples: Vec<u8> = match header.bits_per_color {
        1 => {
            let line_bytes = header.line_bytes();
            let mut out = Vec::with_capacity(w as usize * h as usize);
            for row in data.chunks_exact(line_bytes) {
                out.extend((0..w as usize).map(|x| {
                    let bit = row[x / 8] >> (7 - x % 8) & 1;
                    if bit == 1 { 0xff } else { 0x00 }
                }));
            }
            out
        }
        16 => data.chunks_exact(2).map(|b| b[0]).collect(),
        _ => data,
    };
    let bad = || anyhow::anyhow!("raster page buffer does not match its dimensions");
    Ok(match header.color {
        Color::Gray => image::DynamicImage::ImageLuma8(
            image::GrayImage::from_raw(w, h, samples).ok_or_else(bad)?,
        ),
        Color::Black => image::DynamicImage::ImageLuma8(
            image::GrayImage::from_raw(w, h, samples.into_iter().map(|v| !v).collect())
                .ok_or_else(bad)?,
        ),
        Color::Rgb => image::DynamicImage::ImageRgb8(
            image::RgbImage::from_raw(w, h, samples).ok_or_else(bad)?,
        ),
        Color::Cmyk => {
            let rgb = samples
                .chunks_exact(channels)
                .flat_map(|p| {
                    let k = 255 - u16::from(p[3]);
                    [0, 1, 2].map(|i| ((255 - u16::from(p[i])) * k / 255) as u8)
                })
                .collect();
            image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(w, h, rgb).ok_or_else(bad)?)
        }
    })
}

/// Scales non-square pixels so the page can be placed at one resolution.
fn square_pixels(image: image::DynamicImage, header: &PageHeader) -> image::DynamicImage {
    let (xdpi, ydpi) = header.dpi;
    if xdpi == ydpi {
        return image;
    }
    let height = (u64::from(header.height) * u64::from(xdpi) / u64::from(ydpi)).max(1) as u32;
    image.resize_exact(header.width, height, image::imageops::FilterType::Triangle)
}

/// Reads the next page header, or `None` at the end of the job.
fn next_header(reader: &mut impl Read, flavor: Flavor) -> Result<Option<PageHeader>> {
    match flavor {
        Flavor::Pwg => {
            let mut h = [0u8; PWG_HEADER_LEN];
            if !read_or_eof(reader, &mut h)? {
                return Ok(None);
            }
            parse_pwg_header(&h).map(Some)
        }
        Flavor::Urf => {
            let mut h = [0u8; URF_HEADER_LEN];
            if !read_or_eof(reader, &mut h)? {
                return Ok(None);
            }
            parse_urf_header(&h).map(Some)
        }
    }
}

pub struct RasterImporter {
    max_pages: usize,
}

impl RasterImporter {
    pub fn new(max_pages: usize) -> Self {
        Self { max_pages }
    }
}

impl Default for RasterImporter {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Plugin for RasterImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "print-raster".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: vec!["pwg".into(), "urf".into()],
            mime_types: vec!["image/pwg-raster".into(), "image/urf".into()],
            priority: 50,
        }
    }
}

impl Importer for RasterImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        if Flavor::sniff(&source.path).is_some() {
            return ProbeScore::MAGIC;
        }
        let ext = source
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if self.descriptor().extensions.iter().any(|x| x == &ext) {
            ProbeScore::EXTENSION
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, ctx: &JobContext, mut source: SourceRecord) -> Result<ImportOutcome> {
        let name = anytopdf_core::basename(&source.path);
        let flavor = Flavor::sniff(&source.path)
            .with_context(|| format!("{name}: not a PWG or Apple raster file"))?;
        let mut reader = BufReader::new(
            File::open(&source.path).with_context(|| format!("open {}", source.path.display()))?,
        );
        let mut sync = vec![0u8; if flavor == Flavor::Urf { 12 } else { 4 }];
        reader
            .read_exact(&mut sync)
            .with_context(|| format!("{name}: truncated raster file header"))?;
        let declared = (flavor == Flavor::Urf).then(|| be_u32(&sync, 8) as usize);

        let mut units = Vec::new();
        let mut seen = 0usize;
        let mut failure = None;
        loop {
            if declared.is_some_and(|n| seen >= n) {
                break;
            }
            let page = (|| -> Result<Option<(PageHeader, Vec<u8>)>> {
                let Some(header) = next_header(&mut reader, flavor)? else {
                    return Ok(None);
                };
                let data = decode_page(&mut reader, &header)?;
                Ok(Some((header, data)))
            })();
            let (header, data) = match page {
                Ok(Some(page)) => page,
                Ok(None) => break,
                Err(e) if units.is_empty() => {
                    return Err(e.context(format!("decode {name} page {}", seen + 1)));
                }
                Err(e) => {
                    failure = Some(format!("{e:#}"));
                    break;
                }
            };
            seen += 1;
            if self.max_pages > 0 && units.len() >= self.max_pages {
                continue;
            }
            let image = square_pixels(to_image(&header, data)?, &header);
            let visual = ctx
                .workspace
                .join(format!("raster-{}-{}.png", source.id, units.len()));
            image.save(&visual)?;
            let mut unit = Unit::visual(source.id, visual);
            unit.anchor = Some(Anchor::Region {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                frame: Some(units.len() as u32),
            });
            unit.metadata
                .insert("visual.dpi".into(), header.dpi.0.to_string());
            units.push(unit);
        }
        ensure!(!units.is_empty(), "{name}: raster job contains no pages");
        source
            .metadata
            .insert("raster.format".into(), flavor.name().into());
        source
            .metadata
            .insert("raster.pages".into(), seen.to_string());

        let mut warnings = Vec::new();
        if let Some(reason) = failure {
            warnings.push(
                Diagnostic::new(
                    DiagnosticCode::FramesNotImported,
                    format!(
                        "{name}: imported {} pages; page {} is unreadable: {reason}",
                        units.len(),
                        seen + 1
                    ),
                )
                .to_string(),
            );
        } else if units.len() < seen {
            warnings.push(
                Diagnostic::new(
                    DiagnosticCode::FramesNotImported,
                    format!("{name}: imported {} of {seen} frames", units.len()),
                )
                .to_string(),
            );
        }
        Ok(ImportOutcome {
            source,
            units,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put(h: &mut [u8], offset: usize, value: u32) {
        h[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }

    /// Encodes lines as literal runs, one compressed line per raster line.
    fn encode_lines(lines: &[Vec<u8>], unit: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for line in lines {
            out.push(0);
            for chunk in line.chunks(128 * unit) {
                let pixels = chunk.len() / unit;
                if pixels == 1 {
                    out.push(0);
                } else {
                    out.push((257 - pixels) as u8);
                }
                out.extend_from_slice(chunk);
            }
        }
        out
    }

    fn pwg_page(w: u32, h: u32, dpi: (u32, u32), space: u32, bpc: u32, body: &[u8]) -> Vec<u8> {
        let channels = match space {
            1 | 19 | 20 => 3,
            6 => 4,
            _ => 1,
        };
        let mut header = vec![0u8; PWG_HEADER_LEN];
        put(&mut header, 276, dpi.0);
        put(&mut header, 280, dpi.1);
        put(&mut header, 372, w);
        put(&mut header, 376, h);
        put(&mut header, 384, bpc);
        put(&mut header, 388, bpc * channels);
        put(&mut header, 392, (w * bpc * channels).div_ceil(8));
        put(&mut header, 400, space);
        header.extend_from_slice(body);
        header
    }

    fn pwg_file(pages: &[Vec<u8>]) -> Vec<u8> {
        let mut out = PWG_SYNC.to_vec();
        for page in pages {
            out.extend_from_slice(page);
        }
        out
    }

    /// Width, height, dpi, bits per pixel, colour space and compressed body.
    type UrfPage = (u32, u32, u32, u8, u8, Vec<u8>);

    fn urf_file(pages: &[UrfPage]) -> Vec<u8> {
        let mut out = URF_SYNC.to_vec();
        out.extend_from_slice(&(pages.len() as u32).to_be_bytes());
        for (w, h, dpi, bpp, space, body) in pages {
            let mut header = [0u8; URF_HEADER_LEN];
            header[0] = *bpp;
            header[1] = *space;
            put(&mut header, 12, *w);
            put(&mut header, 16, *h);
            put(&mut header, 20, *dpi);
            out.extend_from_slice(&header);
            out.extend_from_slice(body);
        }
        out
    }

    fn import(bytes: &[u8], file: &str, cap: usize) -> (tempfile::TempDir, Result<ImportOutcome>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(file);
        std::fs::write(&path, bytes).unwrap();
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let outcome = RasterImporter::new(cap).import(&ctx, SourceRecord::new(path));
        (dir, outcome)
    }

    fn pixels(unit: &Unit) -> image::RgbImage {
        image::open(unit.visual_path.as_ref().unwrap())
            .unwrap()
            .to_rgb8()
    }

    #[test]
    fn probe_prefers_magic_over_extension() {
        let dir = tempfile::tempdir().unwrap();
        let magic = dir.path().join("job.bin");
        std::fs::write(&magic, pwg_file(&[])).unwrap();
        let named = dir.path().join("empty.urf");
        std::fs::write(&named, b"").unwrap();
        let other = dir.path().join("notes.txt");
        std::fs::write(&other, b"RaS").unwrap();
        let importer = RasterImporter::default();
        assert_eq!(importer.probe(&SourceRecord::new(magic)), ProbeScore::MAGIC);
        assert_eq!(
            importer.probe(&SourceRecord::new(named)),
            ProbeScore::EXTENSION
        );
        assert_eq!(importer.probe(&SourceRecord::new(other)), ProbeScore::NONE);
    }

    #[test]
    fn pwg_srgb_pages_import_in_order_with_their_resolution() {
        let red = encode_lines(&vec![vec![255, 0, 0, 255, 0, 0]; 2], 3);
        let blue = encode_lines(&vec![vec![0, 0, 255, 0, 0, 255]; 2], 3);
        let bytes = pwg_file(&[
            pwg_page(2, 2, (300, 300), 19, 8, &red),
            pwg_page(2, 2, (300, 300), 19, 8, &blue),
        ]);
        let (_dir, outcome) = import(&bytes, "job.pwg", 0);
        let outcome = outcome.unwrap();
        assert_eq!(outcome.units.len(), 2);
        assert_eq!(
            *pixels(&outcome.units[0]).get_pixel(1, 1),
            image::Rgb([255, 0, 0])
        );
        assert_eq!(
            *pixels(&outcome.units[1]).get_pixel(0, 0),
            image::Rgb([0, 0, 255])
        );
        for (k, unit) in outcome.units.iter().enumerate() {
            assert_eq!(unit.metadata["visual.dpi"], "300");
            assert!(matches!(
                unit.anchor,
                Some(Anchor::Region { frame: Some(f), .. }) if f as usize == k
            ));
        }
        assert_eq!(outcome.source.metadata["raster.format"], "pwg-raster");
        assert_eq!(outcome.source.metadata["raster.pages"], "2");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    }

    #[test]
    fn pwg_repeat_runs_and_white_fill_decode() {
        // Line 1 repeated twice: a run of 3 grey pixels then white fill.
        // Line 3: one literal black pixel then a run of 3 grey pixels.
        let body = [1, 2, 0x40, 0x80, 0, 0, 0x00, 2, 0x40];
        let bytes = pwg_file(&[pwg_page(4, 3, (150, 150), 18, 8, &body)]);
        let (_dir, outcome) = import(&bytes, "job.pwg", 0);
        let image = image::open(outcome.unwrap().units[0].visual_path.as_ref().unwrap())
            .unwrap()
            .to_luma8();
        let rows: Vec<Vec<u8>> = (0..3)
            .map(|y| (0..4).map(|x| image.get_pixel(x, y).0[0]).collect())
            .collect();
        assert_eq!(
            rows,
            [
                vec![0x40, 0x40, 0x40, 0xff],
                vec![0x40, 0x40, 0x40, 0xff],
                vec![0x00, 0x40, 0x40, 0x40]
            ]
        );
    }

    #[test]
    fn pwg_black_and_bilevel_pages_map_to_grey() {
        let black = encode_lines(&[vec![0x00, 0xff]], 1);
        let bilevel = encode_lines(&[vec![0b1000_0000]], 1);
        let bytes = pwg_file(&[
            pwg_page(2, 1, (300, 300), 3, 8, &black),
            pwg_page(2, 1, (300, 300), 18, 1, &bilevel),
        ]);
        let (_dir, outcome) = import(&bytes, "job.pwg", 0);
        let units = outcome.unwrap().units;
        let first = pixels(&units[0]);
        assert_eq!(
            (first.get_pixel(0, 0).0[0], first.get_pixel(1, 0).0[0]),
            (255, 0)
        );
        let second = pixels(&units[1]);
        assert_eq!(
            (second.get_pixel(0, 0).0[0], second.get_pixel(1, 0).0[0]),
            (255, 0)
        );
    }

    #[test]
    fn non_square_resolution_is_resampled_to_square_pixels() {
        let body = encode_lines(&vec![vec![0x80; 4]; 2], 1);
        let bytes = pwg_file(&[pwg_page(4, 2, (600, 300), 18, 8, &body)]);
        let (_dir, outcome) = import(&bytes, "job.pwg", 0);
        let unit = &outcome.unwrap().units[0];
        assert_eq!(pixels(unit).dimensions(), (4, 4));
        assert_eq!(unit.metadata["visual.dpi"], "600");
    }

    #[test]
    fn urf_pages_import_with_declared_count() {
        let gray = encode_lines(&[vec![0x10, 0x20]], 1);
        let rgb = encode_lines(&[vec![0, 255, 0, 0, 255, 0]], 3);
        let bytes = urf_file(&[(2, 1, 600, 8, 0, gray), (2, 1, 600, 24, 1, rgb)]);
        let (_dir, outcome) = import(&bytes, "job.urf", 0);
        let outcome = outcome.unwrap();
        assert_eq!(outcome.units.len(), 2);
        assert_eq!(pixels(&outcome.units[0]).get_pixel(1, 0).0[0], 0x20);
        assert_eq!(
            *pixels(&outcome.units[1]).get_pixel(0, 0),
            image::Rgb([0, 255, 0])
        );
        assert_eq!(outcome.units[1].metadata["visual.dpi"], "600");
        assert_eq!(outcome.source.metadata["raster.format"], "apple-raster");
    }

    #[test]
    fn page_cap_imports_k_and_warns_k_of_n() {
        let line = encode_lines(&[vec![0x80]], 1);
        let page = pwg_page(1, 1, (300, 300), 18, 8, &line);
        let bytes = pwg_file(&[page.clone(), page.clone(), page]);
        let (_dir, outcome) = import(&bytes, "job.pwg", 2);
        let outcome = outcome.unwrap();
        assert_eq!(outcome.units.len(), 2);
        let d = Diagnostic::from_wire(&outcome.warnings[0]);
        assert_eq!(d.code, DiagnosticCode::FramesNotImported);
        assert_eq!(d.message, "job.pwg: imported 2 of 3 frames");
    }

    #[test]
    fn truncated_later_page_keeps_earlier_pages_and_warns() {
        let line = encode_lines(&[vec![0x80]], 1);
        let page = pwg_page(1, 1, (300, 300), 18, 8, &line);
        let mut bytes = pwg_file(&[page.clone(), page]);
        bytes.truncate(bytes.len() - 1);
        let (_dir, outcome) = import(&bytes, "job.pwg", 0);
        let outcome = outcome.unwrap();
        assert_eq!(outcome.units.len(), 1);
        let d = Diagnostic::from_wire(&outcome.warnings[0]);
        assert_eq!(d.code, DiagnosticCode::FramesNotImported);
        assert!(d.message.contains("page 2 is unreadable"), "{}", d.message);
    }

    #[test]
    fn malformed_first_page_is_an_error() {
        let mut page = pwg_page(1, 1, (300, 300), 18, 8, &[0, 0x80]);
        put(&mut page, 392, 99);
        let (_dir, outcome) = import(&pwg_file(&[page]), "job.pwg", 0);
        let error = format!("{:#}", outcome.unwrap_err());
        assert!(error.contains("bytes per line"), "{error}");
    }

    #[test]
    fn oversized_page_is_rejected_before_allocation() {
        let page = pwg_page(1 << 16, 1 << 16, (300, 300), 18, 8, &[]);
        let (_dir, outcome) = import(&pwg_file(&[page]), "job.pwg", 0);
        let error = format!("{:#}", outcome.unwrap_err());
        assert!(error.contains("pixel limit"), "{error}");
    }

    #[test]
    fn run_past_line_end_is_an_error() {
        let page = pwg_page(1, 1, (300, 300), 18, 8, &[0, 0xfe, 1, 2]);
        let (_dir, outcome) = import(&pwg_file(&[page]), "job.pwg", 0);
        let error = format!("{:#}", outcome.unwrap_err());
        assert!(error.contains("overflows"), "{error}");
    }

    #[test]
    fn empty_job_is_an_error() {
        let (_dir, outcome) = import(&pwg_file(&[]), "job.pwg", 0);
        assert!(format!("{:#}", outcome.unwrap_err()).contains("no pages"));
    }
}
