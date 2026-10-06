//! Typed option tables for the built-in plugins.
//!
//! Each struct is one configuration table, such as `[video]`. Keys a
//! table leaves out keep their defaults; unknown keys are errors so typos do
//! not pass silently. A new table needs a struct here, a field on
//! [`BuiltinOptions`], an entry in [`option_tables`] and a `$defs` entry in
//! `schemas/config.schema.json`.

use crate::OcrMode;
use anyhow::Result;
use anytopdf_core::PluginOptions;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// `[image]`: still images and print rasters.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ImageOptions {
    /// Frames to import from a multi-frame TIFF or GIF (0 means unlimited).
    pub max_frames: usize,
}

/// `[video]`: keyframe sampling through FFmpeg.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoOptions {
    /// Seconds between sampled frames.
    pub interval: f64,
    /// Scene-change sensitivity for frame selection.
    pub scene_threshold: f64,
    /// Perceptual-hash distance below which frames count as duplicates.
    pub dedupe_distance: u32,
    /// Frames to keep (0 means unlimited).
    pub max_frames: usize,
}

impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            interval: 5.0,
            scene_threshold: 0.30,
            dedupe_distance: 4,
            max_frames: 0,
        }
    }
}

/// `[ocr]`: the OCR provider chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OcrOptions {
    /// `auto`, `vision`, `doctr`, `tesseract` or `off`.
    pub mode: OcrMode,
    /// OCR language code.
    pub lang: String,
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self {
            mode: OcrMode::Auto,
            lang: "eng".to_string(),
        }
    }
}

/// `[captions]`: sidecar and embedded captions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CaptionOptions {
    /// Read subtitle tracks embedded in video files.
    pub embedded_subtitles: bool,
}

impl Default for CaptionOptions {
    fn default() -> Self {
        Self {
            embedded_subtitles: true,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BuiltinOptions {
    pub image: ImageOptions,
    pub video: VideoOptions,
    pub ocr: OcrOptions,
    pub captions: CaptionOptions,
    /// Transcripts named on the command line for this run.
    pub explicit_transcripts: Vec<PathBuf>,
}

impl BuiltinOptions {
    /// Reads every built-in table from resolved configuration tables.
    pub fn from_tables(tables: &PluginOptions) -> Result<Self> {
        Ok(Self {
            image: tables.get("image")?,
            video: tables.get("video")?,
            ocr: tables.get("ocr")?,
            captions: tables.get("captions")?,
            explicit_transcripts: Vec::new(),
        })
    }
}

/// One built-in configuration table and its defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct OptionTable {
    pub name: &'static str,
    pub defaults: Value,
}

fn table(name: &'static str, defaults: impl Serialize) -> OptionTable {
    OptionTable {
        name,
        defaults: serde_json::to_value(defaults).expect("option defaults serialize"),
    }
}

/// Every built-in table with its defaults, in the order `anytopdf config` prints them.
pub fn option_tables() -> Vec<OptionTable> {
    vec![
        table("image", ImageOptions::default()),
        table("video", VideoOptions::default()),
        table("ocr", OcrOptions::default()),
        table("captions", CaptionOptions::default()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_tables_and_keys_keep_the_defaults() {
        let tables = PluginOptions::from_document(&serde_json::json!({
            "video": {"interval": 2.5},
            "ocr": {"mode": "none"},
        }));
        let options = BuiltinOptions::from_tables(&tables).unwrap();
        assert_eq!(options.video.interval, 2.5);
        assert_eq!(options.video.scene_threshold, 0.30);
        assert_eq!(options.ocr.mode, OcrMode::Off);
        assert_eq!(options.ocr.lang, "eng");
        assert_eq!(options.image, ImageOptions::default());
    }

    #[test]
    fn a_misspelled_key_names_its_table() {
        let tables = PluginOptions::from_document(&serde_json::json!({
            "video": {"intervall": 2.5},
        }));
        let err = BuiltinOptions::from_tables(&tables).unwrap_err();
        let text = format!("{err:#}");
        assert!(
            text.contains("[video]") && text.contains("intervall"),
            "{text}"
        );
    }

    #[test]
    fn defaults_round_trip_through_their_tables() {
        for table in option_tables() {
            assert!(table.defaults.is_object(), "{}", table.name);
        }
        let mut doc = serde_json::Map::new();
        for t in option_tables() {
            doc.insert(t.name.into(), t.defaults);
        }
        let tables = PluginOptions::from_document(&Value::Object(doc));
        assert_eq!(
            BuiltinOptions::from_tables(&tables).unwrap(),
            BuiltinOptions::default()
        );
    }
}
