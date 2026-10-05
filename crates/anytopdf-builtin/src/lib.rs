mod captions;
mod containers;
mod discovery;
pub mod email;
mod html;
mod importers;
mod metadata;
mod ocr;
mod providers;
mod scene;

use anytopdf_core::Registry;
use std::sync::Arc;

pub use discovery::{DiscoveryOptions, discover_inputs};
pub use html::{HtmlText, html_to_text};
pub use ocr::{OcrEnricher, OcrMode, OcrProviderStatus};
pub use providers::{ProviderVersion, detect_providers};

#[derive(Debug, Clone)]
pub struct BuiltinOptions {
    pub video_interval: f64,
    pub scene_threshold: f64,
    pub dedupe_distance: u32,
    pub max_video_frames: usize,
    pub max_image_frames: usize,
    pub ocr: OcrMode,
    pub ocr_language: String,
    pub explicit_transcripts: Vec<std::path::PathBuf>,
    pub embedded_subtitles: bool,
}

impl Default for BuiltinOptions {
    fn default() -> Self {
        Self {
            video_interval: 5.0,
            scene_threshold: 0.30,
            dedupe_distance: 4,
            max_video_frames: 0,
            max_image_frames: 0,
            ocr: OcrMode::Auto,
            ocr_language: "eng".to_string(),
            explicit_transcripts: Vec::new(),
            embedded_subtitles: true,
        }
    }
}

pub fn register_builtins(registry: &mut Registry, opts: BuiltinOptions) {
    registry.register_source_enricher(Arc::new(metadata::MetadataEnricher));

    registry.register_importer(Arc::new(importers::ImageImporter::new(
        opts.max_image_frames,
    )));
    registry.register_importer(Arc::new(importers::RasterImporter::new(
        opts.max_image_frames,
    )));
    registry.register_importer(Arc::new(importers::TextImporter));
    registry.register_importer(Arc::new(importers::HtmlImporter));
    registry.register_importer(Arc::new(importers::EmailImporter));
    registry.register_importer(Arc::new(importers::ArchiveImporter));
    registry.register_importer(Arc::new(importers::SubtitleImporter));
    registry.register_importer(Arc::new(importers::VideoImporter::new(
        opts.video_interval,
        opts.scene_threshold,
        opts.dedupe_distance,
        opts.max_video_frames,
    )));
    registry.register_importer(Arc::new(importers::AudioImporter));
    registry.register_importer(Arc::new(importers::OfficeImporter));

    registry.register_unit_enricher(Arc::new(ocr::OcrEnricher::new(opts.ocr, opts.ocr_language)));
    registry.register_graph_enricher(Arc::new(captions::CaptionEnricher::new(
        opts.explicit_transcripts,
        opts.embedded_subtitles,
    )));
    registry.register_unit_enricher(Arc::new(scene::SceneAnnotationEnricher));
}
