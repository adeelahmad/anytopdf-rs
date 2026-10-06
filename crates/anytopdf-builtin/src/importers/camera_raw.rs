//! Camera RAW photos (CR2, CR3, NEF, ARW, DNG, RAF, ORF, RW2, PEF, ...).
//!
//! Almost every RAW file carries a camera-rendered JPEG preview, usually at
//! full sensor resolution. The importer finds it in-process with a format-blind
//! JPEG scan, so no codec or external tool is needed for the common case. When
//! the preview is missing or small (some DNGs only keep a thumbnail), the RAW
//! data is developed by the first available local tool instead.

use anyhow::{Context, Result, bail};
use anytopdf_core::{
    CommandExt, Diagnostic, DiagnosticCode, ImportOutcome, Importer, JobContext, Plugin,
    PluginDescriptor, ProbeScore, SourceRecord, Unit,
};
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder};
use std::fmt;
use std::fs::File;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::time::Duration;

/// RAW extensions claimed without a recognisable signature. Generic `.raw`
/// is only claimed when the bytes match a known RAW layout.
const RAW_EXTENSIONS: &[&str] = &[
    "3fr", "ari", "arw", "bay", "cap", "cr2", "cr3", "crw", "dcr", "dcs", "dng", "drf", "eip",
    "erf", "fff", "gpr", "iiq", "k25", "kdc", "mdc", "mef", "mos", "mrw", "nef", "nrw", "orf",
    "pef", "ptx", "pxn", "raf", "rw2", "rwl", "rwz", "sr2", "srf", "srw", "x3f",
];

/// Largest RAW file read into memory for preview extraction.
const MAX_RAW_BYTES: u64 = 512 * 1024 * 1024;
/// Previews whose long edge is shorter than this are developed instead, in `auto` mode.
const MIN_PREVIEW_EDGE: u32 = 1600;
const DEVELOP_TIMEOUT: Duration = Duration::from_secs(300);
const TAG_ORIENTATION: u16 = 0x0112;
const TAG_DNG_VERSION: u16 = 0xc612;

/// How a RAW photo becomes a page image.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RawDecode {
    /// Embedded preview when it is large enough, otherwise develop the RAW data.
    #[default]
    Auto,
    /// Always use the embedded camera preview; never run an external tool.
    Preview,
    /// Develop the RAW data with a local tool, falling back to the preview.
    Develop,
}

impl FromStr for RawDecode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "preview" => Ok(Self::Preview),
            "develop" => Ok(Self::Develop),
            other => Err(format!(
                "unknown RAW decode mode {other:?} (expected auto, preview or develop)"
            )),
        }
    }
}

impl fmt::Display for RawDecode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Auto => "auto",
            Self::Preview => "preview",
            Self::Develop => "develop",
        })
    }
}

/// Container layout recognised from the first bytes of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signature {
    /// Plain TIFF header: CR2, NEF, ARW, DNG, PEF, SRW, 3FR, ...
    Tiff,
    /// TIFF variants with their own magic number.
    Orf,
    Rw2,
    /// ISO base media file with the `crx ` brand.
    Cr3,
    Raf,
    Crw,
    Mrw,
    X3f,
}

impl Signature {
    fn sniff(head: &[u8]) -> Option<Self> {
        let at = |range: std::ops::Range<usize>| head.get(range);
        match head.get(..4)? {
            b"II*\0" | b"MM\0*" => return Some(Self::Tiff),
            b"IIRO" | b"IIRS" | b"MMOR" => return Some(Self::Orf),
            b"IIU\0" => return Some(Self::Rw2),
            b"\0MRM" => return Some(Self::Mrw),
            b"FOVb" => return Some(Self::X3f),
            _ => {}
        }
        if at(4..8) == Some(b"ftyp") && at(8..12) == Some(b"crx ") {
            Some(Self::Cr3)
        } else if head.starts_with(b"FUJIFILMCCD-RAW") {
            Some(Self::Raf)
        } else if at(6..14) == Some(b"HEAPCCDR") {
            Some(Self::Crw)
        } else {
            None
        }
    }

    /// Whether the bytes alone identify a RAW photo, without the extension.
    fn is_raw_specific(self) -> bool {
        self != Self::Tiff
    }
}

fn read_head(path: &Path, len: u64) -> Option<Vec<u8>> {
    let mut head = Vec::new();
    File::open(path)
        .ok()?
        .take(len)
        .read_to_end(&mut head)
        .ok()?;
    Some(head)
}

/// Minimal TIFF directory reader for the tags this importer needs.
struct Tiff<'a> {
    data: &'a [u8],
    le: bool,
}

impl<'a> Tiff<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        let le = match data.get(..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        Some(Self { data, le })
    }

    fn u16(&self, offset: usize) -> Option<u16> {
        let b: [u8; 2] = self.data.get(offset..offset + 2)?.try_into().ok()?;
        Some(if self.le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    }

    fn u32(&self, offset: usize) -> Option<u32> {
        let b: [u8; 4] = self.data.get(offset..offset + 4)?.try_into().ok()?;
        Some(if self.le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    }

    /// Finds `tag` in IFD0 and returns its entry offset.
    fn ifd0_entry(&self, tag: u16) -> Option<usize> {
        let ifd = self.u32(4)? as usize;
        let count = self.u16(ifd)? as usize;
        (0..count.min(1024))
            .map(|i| ifd + 2 + i * 12)
            .find(|&entry| self.u16(entry) == Some(tag))
    }

    /// The IFD0 orientation as an EXIF value (1 to 8).
    fn orientation(&self) -> Option<u8> {
        let entry = self.ifd0_entry(TAG_ORIENTATION)?;
        u8::try_from(self.u16(entry + 8)?).ok()
    }

    fn is_dng(&self) -> bool {
        self.ifd0_entry(TAG_DNG_VERSION).is_some()
    }
}

/// Orientation recorded by the RAW container. CR3 keeps its TIFF IFD0 in a
/// `CMT1` box; RAF previews carry their own EXIF, read when decoding.
fn container_orientation(data: &[u8], signature: Signature) -> Option<u8> {
    let tiff = match signature {
        Signature::Tiff | Signature::Orf | Signature::Rw2 => data,
        Signature::Cr3 => {
            let window = &data[..data.len().min(1 << 20)];
            let at = window.windows(4).position(|w| w == b"CMT1")?;
            data.get(at + 4..)?
        }
        _ => return None,
    };
    Tiff::new(tiff)?
        .orientation()
        .filter(|o| (1..=8).contains(o))
}

/// A baseline or progressive JPEG found inside the RAW container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JpegSpan {
    start: usize,
    end: usize,
    width: u32,
    height: u32,
}

impl JpegSpan {
    fn area(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    fn long_edge(&self) -> u32 {
        self.width.max(self.height)
    }
}

/// Walks one JPEG's marker structure from its SOI and returns its extent and
/// frame size. Lossless (SOF3) streams, which hold RAW sensor data rather than
/// a picture, and arithmetic-coded or hierarchical frames are rejected.
fn parse_jpeg(data: &[u8], start: usize) -> Option<JpegSpan> {
    let mut pos = start + 2;
    let mut size = None;
    loop {
        if *data.get(pos)? != 0xff {
            return None;
        }
        while *data.get(pos)? == 0xff {
            pos += 1;
        }
        let marker = *data.get(pos)?;
        pos += 1;
        match marker {
            0xd9 => {
                let (width, height) = size?;
                return Some(JpegSpan {
                    start,
                    end: pos,
                    width,
                    height,
                });
            }
            0x01 | 0xd0..=0xd7 => continue,
            0xd8 => return None,
            _ => {}
        }
        let len = usize::from(u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]));
        if len < 2 {
            return None;
        }
        match marker {
            0xc0..=0xc2 => {
                let height = u16::from_be_bytes([*data.get(pos + 3)?, *data.get(pos + 4)?]);
                let width = u16::from_be_bytes([*data.get(pos + 5)?, *data.get(pos + 6)?]);
                if width == 0 || height == 0 {
                    return None;
                }
                size = Some((u32::from(width), u32::from(height)));
            }
            0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => return None,
            _ => {}
        }
        pos += len;
        if marker == 0xda {
            size?;
            // Entropy-coded data runs to the next marker that is not a stuffed
            // byte or a restart marker.
            loop {
                let next = pos + data.get(pos..)?.iter().position(|&b| b == 0xff)?;
                match *data.get(next + 1)? {
                    0x00 | 0xd0..=0xd7 | 0xff => pos = next + 1,
                    _ => {
                        pos = next;
                        break;
                    }
                }
            }
        }
    }
}

/// Every decodable-looking JPEG in the file, outermost streams only.
fn find_jpegs(data: &[u8]) -> Vec<JpegSpan> {
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(found) = data
        .get(pos..)
        .and_then(|rest| rest.windows(3).position(|w| w == [0xff, 0xd8, 0xff]))
    {
        let start = pos + found;
        match parse_jpeg(data, start) {
            Some(span) => {
                spans.push(span);
                pos = span.end;
            }
            None => pos = start + 1,
        }
    }
    spans
}

/// The largest embedded preview that decodes, with orientation applied.
fn decode_preview(data: &[u8], signature: Signature) -> Option<(DynamicImage, JpegSpan)> {
    let mut spans = find_jpegs(data);
    spans.sort_by_key(|s| std::cmp::Reverse(s.area()));
    let container = container_orientation(data, signature).and_then(Orientation::from_exif);
    spans.into_iter().find_map(|span| {
        let bytes = &data[span.start..span.end];
        let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(bytes)).ok()?;
        let own = decoder.orientation().ok();
        let mut image = DynamicImage::from_decoder(decoder).ok()?;
        image.apply_orientation(container.or(own).unwrap_or(Orientation::NoTransforms));
        Some((image, span))
    })
}

/// Local tools that develop RAW sensor data into an image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Developer {
    /// macOS built-in, backed by Apple's RAW engine.
    Sips,
    /// LibRaw's dcraw emulator.
    DcrawEmu,
    Dcraw,
    /// ImageMagick 7, through its RAW delegate.
    Magick,
    /// ImageMagick 6; skipped on Windows, where `convert` is a disk utility.
    Convert,
}

impl Developer {
    const ALL: [Developer; 5] = [
        Developer::Sips,
        Developer::DcrawEmu,
        Developer::Dcraw,
        Developer::Magick,
        Developer::Convert,
    ];

    fn program(self) -> &'static str {
        match self {
            Developer::Sips => "sips",
            Developer::DcrawEmu => "dcraw_emu",
            Developer::Dcraw => "dcraw",
            Developer::Magick => "magick",
            Developer::Convert => "convert",
        }
    }

    fn usable_here(self) -> bool {
        match self {
            Developer::Sips => cfg!(target_os = "macos"),
            Developer::Convert => !cfg!(windows),
            _ => true,
        }
    }

    /// dcraw-style tools write their TIFF beside the input, so they get a
    /// private copy of it inside the workspace.
    fn writes_beside_input(self) -> bool {
        matches!(self, Developer::DcrawEmu | Developer::Dcraw)
    }

    /// Builds the command; returns it with the path the image will appear at.
    fn command(self, program: &Path, input: &Path, output: &Path) -> (Command, PathBuf) {
        let mut cmd = Command::new(program);
        match self {
            Developer::Sips => {
                cmd.args(["-s", "format", "png"])
                    .arg(input)
                    .arg("--out")
                    .arg(output);
                (cmd, output.to_path_buf())
            }
            // Camera white balance, 8-bit TIFF; both rotate by the RAW orientation.
            Developer::DcrawEmu => {
                cmd.args(["-w", "-T"]).arg(input);
                let mut produced = input.as_os_str().to_owned();
                produced.push(".tiff");
                (cmd, produced.into())
            }
            Developer::Dcraw => {
                cmd.args(["-w", "-T"]).arg(input);
                (cmd, input.with_extension("tiff"))
            }
            Developer::Magick | Developer::Convert => {
                let mut first = input.as_os_str().to_owned();
                first.push("[0]");
                cmd.arg(first).arg("-auto-orient").arg(output);
                (cmd, output.to_path_buf())
            }
        }
    }
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no output")
        .to_string()
}

/// Runs each available developer in turn; returns the image and the tool
/// name, or every failure when none succeeds.
fn develop(ctx: &JobContext, source: &SourceRecord) -> Result<(DynamicImage, &'static str)> {
    let mut failures = Vec::new();
    for developer in Developer::ALL.into_iter().filter(|d| d.usable_here()) {
        let Ok(program) = which::which(developer.program()) else {
            continue;
        };
        let scratch = tempfile::Builder::new()
            .prefix("raw-develop-")
            .tempdir_in(&ctx.workspace)?;
        let input = if developer.writes_beside_input() {
            let ext = source
                .path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("raw");
            let copy = scratch.path().join(format!("input.{ext}"));
            std::fs::copy(&source.path, &copy)
                .with_context(|| format!("copy {} for developing", source.path.display()))?;
            copy
        } else {
            source.path.clone()
        };
        let (mut cmd, produced) =
            developer.command(&program, &input, &scratch.path().join("developed.png"));
        let name = developer.program();
        match cmd.bounded_output(DEVELOP_TIMEOUT) {
            Ok(out) if out.status.success() => match image::open(&produced) {
                Ok(image) => return Ok((image, name)),
                Err(e) => failures.push(format!("{name}: unreadable output: {e}")),
            },
            Ok(out) => failures.push(format!("{name}: {}", first_line(&out.stderr))),
            Err(e) => failures.push(format!("{name}: {e:#}")),
        }
    }
    if failures.is_empty() {
        bail!("developing RAW data requires sips (macOS), LibRaw's dcraw_emu, dcraw or ImageMagick")
    }
    bail!("no RAW developer succeeded: {}", failures.join("; "))
}

pub struct CameraRawImporter {
    decode: RawDecode,
}

impl CameraRawImporter {
    pub fn new(decode: RawDecode) -> Self {
        Self { decode }
    }
}

impl Default for CameraRawImporter {
    fn default() -> Self {
        Self::new(RawDecode::Auto)
    }
}

pub(crate) fn is_raw_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RAW_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

impl Plugin for CameraRawImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "camera-raw".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: RAW_EXTENSIONS.iter().map(|e| e.to_string()).collect(),
            mime_types: [
                "image/x-canon-cr2",
                "image/x-canon-cr3",
                "image/x-adobe-dng",
                "image/x-nikon-nef",
                "image/x-sony-arw",
                "image/x-fuji-raf",
                "image/x-olympus-orf",
                "image/x-panasonic-rw2",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            priority: 60,
        }
    }
}

impl Importer for CameraRawImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        let known_ext = is_raw_extension(&source.path);
        let generic_raw = source
            .path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("raw"));
        let mime = source
            .detected_type
            .as_deref()
            .is_some_and(|m| self.descriptor().mime_types.iter().any(|x| x == m));
        // Many RAW formats are TIFF files, so the generic image importer would
        // otherwise claim them by magic and decode only a thumbnail.
        let head = read_head(&source.path, 64 * 1024).unwrap_or_default();
        match Signature::sniff(&head) {
            Some(sig) if sig.is_raw_specific() || known_ext || generic_raw || mime => {
                ProbeScore::CERTAIN
            }
            Some(Signature::Tiff) if Tiff::new(&head).is_some_and(|t| t.is_dng()) => {
                ProbeScore::CERTAIN
            }
            _ if mime => ProbeScore::MAGIC,
            _ if known_ext => ProbeScore::EXTENSION,
            _ => ProbeScore::NONE,
        }
    }

    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        let name = anytopdf_core::basename(&source.path);
        let size = std::fs::metadata(&source.path)
            .with_context(|| format!("read {}", source.path.display()))?
            .len();
        anyhow::ensure!(
            size <= MAX_RAW_BYTES,
            "{name}: {size} bytes exceeds the {MAX_RAW_BYTES}-byte RAW limit"
        );
        let data = std::fs::read(&source.path)
            .with_context(|| format!("read {}", source.path.display()))?;
        let signature = Signature::sniff(&data).unwrap_or(Signature::Tiff);
        let preview = decode_preview(&data, signature);
        drop(data);

        let want_develop = match self.decode {
            RawDecode::Preview => false,
            RawDecode::Develop => true,
            RawDecode::Auto => preview
                .as_ref()
                .is_none_or(|(_, span)| span.long_edge() < MIN_PREVIEW_EDGE),
        };
        let mut warnings = Vec::new();
        let developed = if want_develop {
            match develop(ctx, &source) {
                Ok(done) => Some(done),
                Err(e) if preview.is_some() => {
                    let (_, span) = preview.as_ref().expect("checked");
                    warnings.push(
                        Diagnostic::new(
                            DiagnosticCode::LossyDecode,
                            format!(
                                "{name}: used the {}x{} embedded preview because the RAW data \
                                 could not be developed ({e:#})",
                                span.width, span.height
                            ),
                        )
                        .to_string(),
                    );
                    None
                }
                Err(e) => return Err(e.context(format!("{name}: no embedded preview found"))),
            }
        } else {
            None
        };

        let (image, decoder, detail) = match (developed, preview) {
            (Some((image, tool)), _) => (image, tool.to_string(), None),
            (None, Some((image, span))) => (
                image,
                "embedded-preview".to_string(),
                Some(format!("{}x{}", span.width, span.height)),
            ),
            (None, None) => bail!("{name}: no embedded preview found"),
        };
        let visual = ctx.workspace.join(format!("image-{}.png", source.id));
        image.save(&visual)?;
        let mut unit = Unit::visual(source.id, visual);
        unit.metadata.insert("raw.decoder".into(), decoder);
        if let Some(detail) = detail {
            unit.metadata.insert("raw.preview-size".into(), detail);
        }
        Ok(ImportOutcome {
            source,
            units: vec![unit],
            warnings,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{BuiltinOptions, register_builtins};
    use anytopdf_core::Registry;
    use image::{Rgb, RgbImage};

    pub(crate) fn jpeg(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
        let img = RgbImage::from_pixel(width, height, Rgb(color));
        let mut out = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
            .unwrap();
        out
    }

    /// Little-endian TIFF with one IFD0 holding `entries` (tag, type SHORT, value).
    fn tiff_header(entries: &[(u16, u16)]) -> Vec<u8> {
        let mut out = b"II*\0".to_vec();
        out.extend(8u32.to_le_bytes());
        out.extend((entries.len() as u16).to_le_bytes());
        for &(tag, value) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(3u16.to_le_bytes());
            out.extend(1u32.to_le_bytes());
            out.extend(u32::from(value).to_le_bytes());
        }
        out.extend(0u32.to_le_bytes());
        out
    }

    /// A lossless-JPEG (SOF3) stream like the sensor data in CR2 and DNG files.
    fn lossless_stream(width: u16, height: u16) -> Vec<u8> {
        let mut out = vec![0xff, 0xd8, 0xff, 0xc3, 0, 11, 14];
        out.extend(height.to_be_bytes());
        out.extend(width.to_be_bytes());
        out.extend([1, 1, 0x11, 0]);
        out.extend([0xff, 0xda, 0, 8, 1, 1, 0, 1, 0, 0]);
        out.extend([0x12; 64]);
        out.extend([0xff, 0xd9]);
        out
    }

    /// A TIFF-based RAW: header, sensor data, thumbnail and full preview.
    pub(crate) fn fake_nef(orientation: u16) -> Vec<u8> {
        let mut data = tiff_header(&[(TAG_ORIENTATION, orientation)]);
        data.extend(lossless_stream(6000, 4000));
        data.extend(jpeg(16, 12, [10, 10, 200]));
        data.extend([0u8; 37]);
        data.extend(jpeg(64, 48, [200, 30, 30]));
        data
    }

    fn import(
        importer: &CameraRawImporter,
        name: &str,
        bytes: &[u8],
    ) -> (ImportOutcome, DynamicImage) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).unwrap();
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let outcome = importer.import(&ctx, SourceRecord::new(path)).unwrap();
        let image = image::open(outcome.units[0].visual_path.as_ref().unwrap()).unwrap();
        (outcome, image)
    }

    #[test]
    fn preview_scan_picks_the_largest_picture_and_skips_lossless_sensor_data() {
        let data = fake_nef(1);
        let spans = find_jpegs(&data);
        assert_eq!(
            spans
                .iter()
                .map(|s| (s.width, s.height))
                .collect::<Vec<_>>(),
            [(16, 12), (64, 48)]
        );
        let (image, span) = decode_preview(&data, Signature::Tiff).unwrap();
        assert_eq!((image.width(), image.height(), span.width), (64, 48, 64));
        let px = image.to_rgb8().get_pixel(32, 24).0;
        assert!(px[0] > 150 && px[2] < 80, "{px:?}");
    }

    #[test]
    fn container_orientation_rotates_the_preview() {
        let importer = CameraRawImporter::new(RawDecode::Preview);
        let (outcome, image) = import(&importer, "DSC_0001.NEF", &fake_nef(6));
        assert_eq!((image.width(), image.height()), (48, 64));
        let meta = &outcome.units[0].metadata;
        assert_eq!(meta["raw.decoder"], "embedded-preview");
        assert_eq!(meta["raw.preview-size"], "64x48");
        assert!(outcome.warnings.is_empty());
    }

    #[test]
    fn cr3_orientation_comes_from_its_cmt1_box() {
        let mut data = vec![0, 0, 0, 24];
        data.extend(b"ftypcrx \0\0\0\x01crx isom");
        data.extend([0, 0, 0, 40]);
        data.extend(b"CMT1");
        data.extend(tiff_header(&[(TAG_ORIENTATION, 8)]));
        data.extend(jpeg(40, 20, [0, 200, 0]));
        assert_eq!(Signature::sniff(&data), Some(Signature::Cr3));
        let (image, _) = decode_preview(&data, Signature::Cr3).unwrap();
        assert_eq!((image.width(), image.height()), (20, 40));
    }

    #[test]
    fn probe_claims_tiff_based_raw_ahead_of_the_image_importer() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::default();
        register_builtins(&mut registry, BuiltinOptions::default());
        let nef = dir.path().join("DSC_0001.NEF");
        std::fs::write(&nef, fake_nef(1)).unwrap();
        let mut source = SourceRecord::new(nef);
        source.detected_type = Some("image/tiff".into());
        assert_eq!(
            CameraRawImporter::default().probe(&source),
            ProbeScore::CERTAIN
        );
        assert_eq!(
            registry.importer_for(&source).unwrap().descriptor().name,
            "camera-raw"
        );

        // A DNG is recognised by its DNGVersion tag even without an extension.
        let dng = dir.path().join("download");
        std::fs::write(&dng, tiff_header(&[(TAG_DNG_VERSION, 1)])).unwrap();
        assert_eq!(
            CameraRawImporter::default().probe(&SourceRecord::new(dng)),
            ProbeScore::CERTAIN
        );

        // Plain TIFFs and unrelated `.raw` dumps stay with other importers.
        let tif = dir.path().join("scan.tif");
        std::fs::write(&tif, tiff_header(&[(TAG_ORIENTATION, 1)])).unwrap();
        assert_eq!(
            CameraRawImporter::default().probe(&SourceRecord::new(tif)),
            ProbeScore::NONE
        );
        let dump = dir.path().join("pcm.raw");
        std::fs::write(&dump, [0u8; 64]).unwrap();
        assert_eq!(
            CameraRawImporter::default().probe(&SourceRecord::new(dump)),
            ProbeScore::NONE
        );
        let raf = dir.path().join("x.bin");
        std::fs::write(&raf, b"FUJIFILMCCD-RAW 0201FF383501").unwrap();
        assert_eq!(
            CameraRawImporter::default().probe(&SourceRecord::new(raf)),
            ProbeScore::CERTAIN
        );
    }

    #[test]
    fn auto_mode_keeps_a_small_preview_with_a_warning_when_no_developer_succeeds() {
        // The 64x48 preview is below MIN_PREVIEW_EDGE, so auto tries to develop
        // the fake sensor data; every real developer fails on it.
        let importer = CameraRawImporter::new(RawDecode::Auto);
        let (outcome, image) = import(&importer, "IMG_0001.CR2", &fake_nef(1));
        assert_eq!((image.width(), image.height()), (64, 48));
        assert_eq!(outcome.units[0].metadata["raw.decoder"], "embedded-preview");
        assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
        assert!(outcome.warnings[0].contains("64x48 embedded preview"));
    }

    #[test]
    fn a_raw_without_preview_or_developer_fails_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.nef");
        let mut data = tiff_header(&[(TAG_ORIENTATION, 1)]);
        data.extend(lossless_stream(100, 100));
        std::fs::write(&path, data).unwrap();
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let err = CameraRawImporter::new(RawDecode::Preview)
            .import(&ctx, SourceRecord::new(path))
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("no embedded preview"),
            "{err:#}"
        );
    }

    #[test]
    fn developer_commands_write_inside_the_scratch_directory() {
        let input = Path::new("ws/input.nef");
        let output = Path::new("ws/developed.png");
        let run = |d: Developer| {
            let (cmd, produced) = d.command(Path::new(d.program()), input, output);
            let args: Vec<String> = cmd
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            (args, produced)
        };
        assert_eq!(
            run(Developer::Dcraw),
            (
                vec!["-w".into(), "-T".into(), "ws/input.nef".into()],
                "ws/input.tiff".into()
            )
        );
        assert_eq!(
            run(Developer::DcrawEmu).1,
            PathBuf::from("ws/input.nef.tiff")
        );
        assert_eq!(
            run(Developer::Magick).0,
            ["ws/input.nef[0]", "-auto-orient", "ws/developed.png"]
        );
        assert_eq!(run(Developer::Sips).1, PathBuf::from("ws/developed.png"));
    }

    #[test]
    fn decode_modes_parse_case_insensitively() {
        assert_eq!("Develop".parse::<RawDecode>(), Ok(RawDecode::Develop));
        assert_eq!(RawDecode::default().to_string(), "auto");
        assert!("full".parse::<RawDecode>().is_err());
    }
}
