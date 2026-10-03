use anyhow::{Context, Result};
use anytopdf_core::{
    ImportOutcome, Importer, JobContext, Plugin, PluginDescriptor, ProbeScore, SourceRecord, Unit,
};
use image::ImageDecoder;

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
        let mut decoder = image::ImageReader::open(&source.path)?
            .with_guessed_format()?
            .into_decoder()
            .with_context(|| format!("decode {}", source.path.display()))?;
        let orientation = decoder.orientation()?;
        let mut image = image::DynamicImage::from_decoder(decoder)?;
        image.apply_orientation(orientation);
        let visual = ctx.workspace.join(format!("image-{}.png", source.id));
        image.save(&visual)?;
        let unit = Unit::visual(source.id, visual);
        Ok(ImportOutcome {
            source,
            units: vec![unit],
            warnings: vec![],
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
}
