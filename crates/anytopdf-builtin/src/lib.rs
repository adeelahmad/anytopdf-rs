mod captions;
pub mod capture;
mod chat;
mod colors;
mod containers;
mod dates;
mod discovery;
pub mod email;
mod entities;
mod html;
mod importers;
mod location;
mod metadata;
mod ocr;
mod options;
mod providers;
mod scan;
mod scene;
mod structured;

use anytopdf_core::Registry;
use std::sync::Arc;

/// FFmpeg and ffprobe may open only local files named by an input, so a crafted container or
/// playlist cannot make them fetch network URLs.
pub(crate) const FFMPEG_PROTOCOLS: &str = "file";

pub use chat::{ChatDateOrder, ChatOptions};
pub use colors::{DominantColor, dominant_colors};
pub use dates::DateOrder;
pub use discovery::{DiscoveryOptions, discover_inputs};
pub use html::{HtmlText, html_to_text};
pub use importers::RawDecode;
pub use importers::StructuredOptions;
pub use location::{LocationEnricher, LocationMode};
pub use ocr::{OcrEnricher, OcrMode, OcrProviderStatus};
pub use options::{
    BuiltinOptions, CaptionOptions, ColorOptions, EntityOptions, ImageOptions, LocationOptions,
    OcrOptions, OptionTable, RawOptions, ScanOptions, VideoOptions, option_tables,
};
pub use providers::{
    PROVIDER_NAMES, ProviderVersion, URL_PROVIDERS, chrome_path, detect_providers,
    detect_providers_named, ytdlp_path,
};
pub use scan::ScanMode;

pub fn register_builtins(registry: &mut Registry, opts: BuiltinOptions) {
    registry.register_source_enricher(Arc::new(metadata::MetadataEnricher));

    registry.register_importer(Arc::new(importers::ImageImporter::new(
        opts.image.max_frames,
    )));
    registry.register_importer(Arc::new(importers::HeifImporter));
    registry.register_importer(Arc::new(importers::CameraRawImporter::new(opts.raw_decode)));
    registry.register_importer(Arc::new(importers::PdfInputImporter));
    registry.register_importer(Arc::new(importers::RasterImporter::new(
        opts.image.max_frames,
    )));
    registry.register_importer(Arc::new(importers::TextImporter));
    registry.register_importer(Arc::new(importers::StructuredImporter::new(
        opts.structured.clone(),
    )));
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
    let chat_attachments = importers::ImportedAttachments::default();
    registry.register_importer(Arc::new(importers::ChatImporter::new(
        opts.chat,
        chat_attachments.clone(),
    )));

    // Page detection and deskew must change the page image before OCR reads it.
    registry.register_unit_enricher(Arc::new(scan::ScanEnricher::new(opts.scan)));
    registry.register_unit_enricher(Arc::new(ocr::OcrEnricher::new(
        opts.ocr.mode,
        opts.ocr.lang,
    )));
    registry.register_graph_enricher(Arc::new(importers::ChatAttachmentDedupe::new(
        chat_attachments,
    )));
    registry.register_graph_enricher(Arc::new(captions::CaptionEnricher::new(
        opts.explicit_transcripts,
        opts.captions.embedded_subtitles,
    )));
    registry.register_unit_enricher(Arc::new(colors::ColorEnricher::new(opts.colors)));
    registry.register_unit_enricher(Arc::new(scene::SceneAnnotationEnricher));
    // Late, so place names also come from runtime plugins' captions and transcripts.
    registry.register_late_unit_enricher(Arc::new(location::LocationEnricher::new(opts.location)));
    if opts.entities {
        // Late, so entities also come from runtime plugins' captions and transcripts.
        registry.register_late_unit_enricher(Arc::new(entities::EntityEnricher {
            date_order: opts.date_order,
        }));
    }
}
