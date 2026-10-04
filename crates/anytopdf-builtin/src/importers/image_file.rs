use anyhow::{Context, Result};
use anytopdf_core::{
    Diagnostic, DiagnosticCode, ImportOutcome, Importer, JobContext, Plugin, PluginDescriptor,
    ProbeScore, SourceRecord, Unit,
};
use image::ImageDecoder;
use std::io::BufReader;
use std::path::Path;

fn tiff_ifd_count(path: &Path) -> Option<usize> {
    let data = std::fs::read(path).ok()?;
    let le = match data.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| {
        let b: [u8; 2] = data.get(o..o + 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |o: usize| {
        let b: [u8; 4] = data.get(o..o + 4)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let mut offset = u32_at(4)? as usize;
    let mut count = 0;
    while offset != 0 && count < 4096 {
        let entries = u16_at(offset)? as usize;
        count += 1;
        offset = u32_at(offset + 2 + entries * 12)? as usize;
    }
    Some(count)
}

fn frame_count(path: &Path, format: Option<image::ImageFormat>) -> usize {
    use image::AnimationDecoder;
    match format {
        Some(image::ImageFormat::Tiff) => tiff_ifd_count(path).unwrap_or(1),
        Some(image::ImageFormat::Gif) => std::fs::File::open(path)
            .ok()
            .and_then(|f| image::codecs::gif::GifDecoder::new(BufReader::new(f)).ok())
            .map_or(1, |d| d.into_frames().count()),
        _ => 1,
    }
}

pub struct ImageImporter {
    max_frames: usize,
}

impl ImageImporter {
    pub fn new(max_frames: usize) -> Self {
        Self { max_frames }
    }
}

impl Default for ImageImporter {
    fn default() -> Self {
        Self::new(0)
    }
}

impl Plugin for ImageImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "image".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: ["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            mime_types: [
                "image/jpeg",
                "image/png",
                "image/gif",
                "image/webp",
                "image/bmp",
                "image/tiff",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            priority: 50,
        }
    }
}

impl Importer for ImageImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        if source.detected_type.as_deref().is_some_and(|m| {
            self.descriptor()
                .mime_types
                .iter()
                .any(|supported| supported == m)
        }) {
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

    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        let reader = image::ImageReader::open(&source.path)?.with_guessed_format()?;
        let frames = frame_count(&source.path, reader.format());
        // SUB-AGENT-TODO: import min(frames, self.max_frames) frames (0 = unlimited) per tasks.md T2
        let _cap = self.max_frames;
        let mut decoder = reader
            .into_decoder()
            .with_context(|| format!("decode {}", source.path.display()))?;
        let orientation = decoder.orientation()?;
        let mut image = image::DynamicImage::from_decoder(decoder)?;
        image.apply_orientation(orientation);
        let visual = ctx.workspace.join(format!("image-{}.png", source.id));
        image.save(&visual)?;
        let unit = Unit::visual(source.id, visual);
        let warnings = if frames > 1 {
            let name = anytopdf_core::basename(&source.path);
            vec![
                Diagnostic::new(
                    DiagnosticCode::FramesNotImported,
                    format!("{name}: imported 1 of {frames} frames"),
                )
                .to_string(),
            ]
        } else {
            vec![]
        };
        Ok(ImportOutcome {
            source,
            units: vec![unit],
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imports_by_magic_bytes_and_normalizes_into_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let source_path = dir.path().join("image.unknown");
        image::RgbImage::new(4, 3)
            .save_with_format(&source_path, image::ImageFormat::Png)
            .unwrap();
        let source = SourceRecord::new(source_path.clone());
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let imported = ImageImporter::new(1).import(&ctx, source).unwrap();
        let visual = imported.units[0].visual_path.as_ref().unwrap();
        assert_ne!(visual, &source_path);
        assert_eq!(image::image_dimensions(visual).unwrap(), (4, 3));
        assert!(source_path.exists());
    }

    fn tiff_with_ifds(count: usize) -> Vec<u8> {
        const IFD_LEN: usize = 2 + 9 * 12 + 4;
        let pixel_offset = (8 + IFD_LEN * count) as u32;
        let mut out = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        for i in 0..count {
            out.extend_from_slice(&9u16.to_le_bytes());
            for (tag, kind, value) in [
                (256u16, 3u16, 1u32),
                (257, 3, 1),
                (258, 3, 8),
                (259, 3, 1),
                (262, 3, 1),
                (273, 4, pixel_offset),
                (277, 3, 1),
                (278, 3, 1),
                (279, 4, 1),
            ] {
                out.extend_from_slice(&tag.to_le_bytes());
                out.extend_from_slice(&kind.to_le_bytes());
                out.extend_from_slice(&1u32.to_le_bytes());
                out.extend_from_slice(&value.to_le_bytes());
            }
            let next = if i + 1 < count {
                (8 + IFD_LEN * (i + 1)) as u32
            } else {
                0
            };
            out.extend_from_slice(&next.to_le_bytes());
        }
        out.push(0x80);
        out
    }

    fn import_file_capped(path: std::path::PathBuf, cap: usize) -> ImportOutcome {
        let ctx = JobContext {
            workspace: path.parent().unwrap().into(),
            quiet: true,
        };
        ImageImporter::new(cap)
            .import(&ctx, SourceRecord::new(path))
            .unwrap()
    }

    fn import_file(path: std::path::PathBuf) -> ImportOutcome {
        import_file_capped(path, 1)
    }

    fn tiff_with_pages_at(pages: &[(u8, Option<u32>)]) -> Vec<u8> {
        const IFD_LEN: usize = 2 + 9 * 12 + 4;
        let pixels_at = (8 + IFD_LEN * pages.len()) as u32;
        let mut out = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
        for (i, (_, offset)) in pages.iter().enumerate() {
            out.extend_from_slice(&9u16.to_le_bytes());
            for (tag, kind, value) in [
                (256u16, 3u16, 1u32),
                (257, 3, 1),
                (258, 3, 8),
                (259, 3, 1),
                (262, 3, 1),
                (273, 4, offset.unwrap_or(pixels_at + i as u32)),
                (277, 3, 1),
                (278, 3, 1),
                (279, 4, 1),
            ] {
                out.extend_from_slice(&tag.to_le_bytes());
                out.extend_from_slice(&kind.to_le_bytes());
                out.extend_from_slice(&1u32.to_le_bytes());
                out.extend_from_slice(&value.to_le_bytes());
            }
            let next = if i + 1 < pages.len() {
                (8 + IFD_LEN * (i + 1)) as u32
            } else {
                0
            };
            out.extend_from_slice(&next.to_le_bytes());
        }
        out.extend(pages.iter().map(|(v, _)| *v));
        out
    }

    fn tiff_with_pages(values: &[u8]) -> Vec<u8> {
        let pages: Vec<_> = values.iter().map(|v| (*v, None)).collect();
        tiff_with_pages_at(&pages)
    }

    fn assert_frame_anchors(outcome: &ImportOutcome, expected: usize) {
        assert_eq!(outcome.units.len(), expected);
        for (k, unit) in outcome.units.iter().enumerate() {
            match &unit.anchor {
                Some(anytopdf_core::Anchor::Region {
                    x,
                    y,
                    width,
                    height,
                    frame,
                }) => {
                    assert_eq!((*x, *y, *width, *height), (0.0, 0.0, 1.0, 1.0));
                    assert_eq!(*frame, Some(k as u32));
                }
                other => panic!("unit {k}: expected region anchor, got {other:?}"),
            }
        }
    }

    fn assert_distinct_workspace_files(outcome: &ImportOutcome, workspace: &Path) {
        let paths: std::collections::BTreeSet<_> = outcome
            .units
            .iter()
            .map(|u| u.visual_path.clone().unwrap())
            .collect();
        assert_eq!(paths.len(), outcome.units.len());
        for p in paths {
            assert!(p.starts_with(workspace) && p.exists(), "{}", p.display());
        }
    }

    fn first_pixel(unit: &Unit) -> image::Rgba<u8> {
        *image::open(unit.visual_path.as_ref().unwrap())
            .unwrap()
            .to_rgba8()
            .get_pixel(0, 0)
    }

    #[test]
    fn three_ifd_tiff_imports_every_page_in_file_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.tif");
        std::fs::write(&path, tiff_with_pages(&[10, 20, 30])).unwrap();
        let outcome = import_file_capped(path, 0);
        assert_eq!(outcome.units.len(), 3);
        assert_distinct_workspace_files(&outcome, dir.path());
        let lumas: Vec<u8> = outcome.units.iter().map(|u| first_pixel(u).0[0]).collect();
        assert_eq!(lumas, [10, 20, 30]);
        assert_frame_anchors(&outcome, 3);
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    }

    #[test]
    fn two_frame_gif_imports_both_frames_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anim.gif");
        let frames = (0..2u8).map(|n| {
            image::Frame::new(image::RgbaImage::from_pixel(
                2,
                2,
                image::Rgba([n * 100, 0, 0, 255]),
            ))
        });
        let mut encoder =
            image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap());
        encoder.encode_frames(frames).unwrap();
        drop(encoder);
        let outcome = import_file_capped(path, 0);
        assert_eq!(outcome.units.len(), 2);
        assert_distinct_workspace_files(&outcome, dir.path());
        assert_frame_anchors(&outcome, 2);
        for (unit, want) in outcome.units.iter().zip([0i16, 100]) {
            let red = i16::from(first_pixel(unit).0[0]);
            assert!((red - want).abs() <= 2, "red {red} vs {want}");
        }
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    }

    #[test]
    fn frame_cap_imports_k_and_warns_k_of_n() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.tif");
        std::fs::write(&path, tiff_with_pages(&[10, 20, 30])).unwrap();
        let outcome = import_file_capped(path, 2);
        assert_eq!(outcome.units.len(), 2);
        assert_frame_anchors(&outcome, 2);
        assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
        let d = Diagnostic::from_wire(&outcome.warnings[0]);
        assert_eq!(d.code, DiagnosticCode::FramesNotImported);
        assert_eq!(d.message, "scan.tif: imported 2 of 3 frames");
    }

    #[test]
    fn later_frame_decode_failure_keeps_earlier_frames_and_warns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.tif");
        let bytes = tiff_with_pages_at(&[(10, None), (20, None), (30, Some(1_000_000))]);
        std::fs::write(&path, bytes).unwrap();
        let outcome = import_file_capped(path, 0);
        assert_eq!(outcome.units.len(), 2);
        assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
        let d = Diagnostic::from_wire(&outcome.warnings[0]);
        assert_eq!(d.code, DiagnosticCode::FramesNotImported);
        assert!(
            d.message.contains("imported 2 of 3 frames"),
            "{}",
            d.message
        );
    }

    #[test]
    fn single_frame_tiff_keeps_the_existing_path_and_no_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.tif");
        std::fs::write(&path, tiff_with_pages(&[10])).unwrap();
        let outcome = import_file_capped(path, 0);
        assert_eq!(outcome.units.len(), 1);
        assert!(outcome.units[0].anchor.is_none());
        let visual = outcome.units[0].visual_path.as_ref().unwrap();
        let expected = format!("image-{}.png", outcome.source.id);
        assert_eq!(visual.file_name().unwrap().to_str().unwrap(), expected);
        assert!(outcome.warnings.is_empty());
    }

    fn assert_frames_warning(outcome: &ImportOutcome, ratio: &str) {
        use anytopdf_core::{Diagnostic, DiagnosticCode};
        assert_eq!(outcome.units.len(), 1);
        assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
        let d = Diagnostic::from_wire(&outcome.warnings[0]);
        assert_eq!(d.code, DiagnosticCode::FramesNotImported);
        assert!(d.message.contains(ratio), "{}", d.message);
    }

    #[test]
    fn three_page_tiff_warns_frames_not_imported() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scan.tif");
        std::fs::write(&path, tiff_with_ifds(3)).unwrap();
        let outcome = import_file(path);
        assert_frames_warning(&outcome, "1 of 3");
        assert!(outcome.warnings[0].contains("scan.tif"));
    }

    #[test]
    fn animated_gif_warns_with_both_frame_counts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("anim.gif");
        let frames = (0..2u8).map(|n| {
            image::Frame::new(image::RgbaImage::from_pixel(
                2,
                2,
                image::Rgba([n * 100, 0, 0, 255]),
            ))
        });
        let mut encoder =
            image::codecs::gif::GifEncoder::new(std::fs::File::create(&path).unwrap());
        encoder.encode_frames(frames).unwrap();
        drop(encoder);
        let outcome = import_file(path);
        assert_frames_warning(&outcome, "1 of 2");
    }

    #[test]
    fn single_frame_png_has_no_frame_warning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.png");
        image::RgbImage::new(4, 3)
            .save_with_format(&path, image::ImageFormat::Png)
            .unwrap();
        let outcome = import_file(path);
        assert_eq!(outcome.units.len(), 1);
        assert!(outcome.warnings.is_empty());
    }
}
