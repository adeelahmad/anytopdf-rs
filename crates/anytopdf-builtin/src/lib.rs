mod captions;
pub mod capture;
mod containers;
mod discovery;
pub mod email;
mod html;
mod importers;
mod metadata;
mod ocr;
mod options;
mod providers;
mod scene;

use anytopdf_core::Registry;
use std::sync::Arc;

pub use discovery::{DiscoveryOptions, discover_inputs};
pub use html::{HtmlText, html_to_text};
pub use ocr::{OcrEnricher, OcrMode, OcrProviderStatus};
pub use options::{
    BuiltinOptions, CaptionOptions, ImageOptions, OcrOptions, OptionTable, VideoOptions,
    option_tables,
};
pub use providers::{ProviderVersion, detect_providers};

pub fn register_builtins(registry: &mut Registry, opts: BuiltinOptions) {
    registry.register_source_enricher(Arc::new(metadata::MetadataEnricher));

    registry.register_importer(Arc::new(importers::ImageImporter::new(
        opts.image.max_frames,
    )));
    registry.register_importer(Arc::new(importers::HeifImporter));
    registry.register_importer(Arc::new(importers::PdfInputImporter));
    registry.register_importer(Arc::new(importers::RasterImporter::new(
        opts.image.max_frames,
    )));
    registry.register_importer(Arc::new(importers::TextImporter));
    registry.register_importer(Arc::new(importers::HtmlImporter));
    registry.register_importer(Arc::new(importers::EmailImporter));
    registry.register_importer(Arc::new(importers::ArchiveImporter));
    registry.register_importer(Arc::new(importers::SubtitleImporter));
    registry.register_importer(Arc::new(importers::VideoImporter::new(
        opts.video.interval,
        opts.video.scene_threshold,
        opts.video.dedupe_distance,
        opts.video.max_frames,
    )));
    registry.register_importer(Arc::new(importers::AudioImporter));
    registry.register_importer(Arc::new(importers::OfficeImporter));

    registry.register_unit_enricher(Arc::new(ocr::OcrEnricher::new(
        opts.ocr.mode,
        opts.ocr.lang,
    )));
    registry.register_graph_enricher(Arc::new(captions::CaptionEnricher::new(
        opts.explicit_transcripts,
        opts.captions.embedded_subtitles,
    )));
    registry.register_unit_enricher(Arc::new(scene::SceneAnnotationEnricher));
}
