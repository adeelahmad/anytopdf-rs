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

pub struct ImageImporter;

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
            let name = source
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy();
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
        let imported = ImageImporter.import(&ctx, source).unwrap();
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

    fn import_file(path: std::path::PathBuf) -> ImportOutcome {
        let ctx = JobContext {
            workspace: path.parent().unwrap().into(),
            quiet: true,
        };
        ImageImporter.import(&ctx, SourceRecord::new(path)).unwrap()
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
