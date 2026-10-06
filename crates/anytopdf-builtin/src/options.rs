//! Typed option tables for the built-in plugins.
//!
//! Each struct is one configuration table, such as `[video]`. Keys a
//! table leaves out keep their defaults; unknown keys are errors so typos do
//! not pass silently. A new table needs a struct here, a field on
//! [`BuiltinOptions`], an entry in [`option_tables`] and a `$defs` entry in
//! `schemas/config.schema.json`.

use crate::{
    ChatOptions, DateOrder, LocationMode, OcrMode, RawDecode, ScanMode, StructuredOptions,
};
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

/// Reads and writes a mode enum as its configuration string.
macro_rules! string_option {
    ($ty:ty { $($variant:path => $text:literal),+ $(,)? }) => {
        impl Serialize for $ty {
            fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
                s.serialize_str(match self { $($variant => $text),+ })
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
                String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

string_option!(DateOrder { DateOrder::DayFirst => "dmy", DateOrder::MonthFirst => "mdy" });
string_option!(LocationMode { LocationMode::On => "on", LocationMode::Gps => "gps", LocationMode::Off => "off" });
string_option!(RawDecode { RawDecode::Auto => "auto", RawDecode::Preview => "preview", RawDecode::Develop => "develop" });
string_option!(ScanMode { ScanMode::Auto => "auto", ScanMode::On => "on", ScanMode::Off => "off" });

/// `[entities]`: URLs, emails, domains, app names, dates and times in text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EntityOptions {
    /// Extract entities from OCR, captions, transcripts and text pages.
    pub enabled: bool,
    /// Day and month order for all-numeric dates such as `03/04/2024`.
    pub date_order: DateOrder,
}

impl Default for EntityOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            date_order: DateOrder::DayFirst,
        }
    }
}

/// `[colors]`: dominant-colour annotations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ColorOptions {
    /// Annotate visual units with their dominant colours.
    pub enabled: bool,
}

impl Default for ColorOptions {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// `[location]`: GPS fixes and place names.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocationOptions {
    /// `on`, `gps` or `off`.
    pub mode: LocationMode,
}

/// `[raw]`: camera RAW photos.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RawOptions {
    /// `auto`, `preview` or `develop`.
    pub decode: RawDecode,
}

/// `[scan]`: photographed-page cleanup before OCR.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScanOptions {
    /// `auto`, `on` or `off`.
    pub mode: ScanMode,
}

#[derive(Debug, Clone)]
pub struct BuiltinOptions {
    pub image: ImageOptions,
    pub video: VideoOptions,
    pub ocr: OcrOptions,
    pub captions: CaptionOptions,
    /// Transcripts named on the command line for this run.
    pub explicit_transcripts: Vec<PathBuf>,
    // Flattened from the `[entities]`, `[colors]`, `[raw]`, `[scan]` and
    // `[location]` tables.
    /// Extract URLs, emails, domains, app names, dates and times from text.
    pub entities: bool,
    /// Day and month order for all-numeric dates such as `03/04/2024`.
    pub date_order: DateOrder,
    /// Annotate visual units with their dominant colours.
    pub colors: bool,
    /// JSON / JSON Lines importer options.
    pub structured: StructuredOptions,
    pub chat: ChatOptions,
    pub raw_decode: RawDecode,
    pub scan: ScanMode,
    pub location: LocationMode,
}

impl Default for BuiltinOptions {
    fn default() -> Self {
        Self {
            image: ImageOptions::default(),
            video: VideoOptions::default(),
            ocr: OcrOptions::default(),
            captions: CaptionOptions::default(),
            explicit_transcripts: Vec::new(),
            entities: true,
            date_order: DateOrder::DayFirst,
            colors: true,
            structured: StructuredOptions::default(),
            chat: ChatOptions::default(),
            raw_decode: RawDecode::Auto,
            scan: ScanMode::Auto,
            location: LocationMode::default(),
        }
    }
}

impl BuiltinOptions {
    /// Reads every built-in table from resolved configuration tables.
    pub fn from_tables(tables: &PluginOptions) -> Result<Self> {
        Self {
            image: tables.get("image")?,
            video: tables.get("video")?,
            ocr: tables.get("ocr")?,
            captions: tables.get("captions")?,
            ..Self::default()
        }
        .with_flat_tables(tables)
    }
}

impl BuiltinOptions {
    fn with_flat_tables(self, tables: &PluginOptions) -> Result<Self> {
        let entities: EntityOptions = tables.get("entities")?;
        let colors: ColorOptions = tables.get("colors")?;
        let location: LocationOptions = tables.get("location")?;
        let raw: RawOptions = tables.get("raw")?;
        let scan: ScanOptions = tables.get("scan")?;
        Ok(Self {
            entities: entities.enabled,
            date_order: entities.date_order,
            colors: colors.enabled,
            location: location.mode,
            raw_decode: raw.decode,
            scan: scan.mode,
            ..self
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
        table("entities", EntityOptions::default()),
        table("colors", ColorOptions::default()),
        table("location", LocationOptions::default()),
        table("raw", RawOptions::default()),
        table("scan", ScanOptions::default()),
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
        assert!(options.entities && options.colors);
        assert_eq!(options.location, LocationMode::On);
    }

    #[test]
    fn flag_only_settings_read_their_tables() {
        let tables = PluginOptions::from_document(&serde_json::json!({
            "entities": {"enabled": false, "date_order": "mdy"},
            "colors": {"enabled": false},
            "location": {"mode": "gps"},
            "raw": {"decode": "preview"},
            "scan": {"mode": "off"},
        }));
        let options = BuiltinOptions::from_tables(&tables).unwrap();
        assert!(!options.entities && !options.colors);
        assert_eq!(options.date_order, DateOrder::MonthFirst);
        assert_eq!(options.location, LocationMode::Gps);
        assert_eq!(options.raw_decode, RawDecode::Preview);
        assert_eq!(options.scan, ScanMode::Off);
        let bad = PluginOptions::from_document(&serde_json::json!({"scan": {"mode": "sideways"}}));
        assert!(BuiltinOptions::from_tables(&bad).is_err());
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
        let parsed = BuiltinOptions::from_tables(&tables).unwrap();
        let defaults = BuiltinOptions::default();
        assert_eq!(format!("{:?}", parsed), format!("{:?}", defaults));
    }
}
