//! `unit-enrich`: describes one image or video keyframe.

use crate::client::{CallError, Client};
use crate::config::Config;
use anyhow::{Context, Result};
use base64::Engine as _;
use serde_json::{Map, Value, json};
use std::{io::Cursor, path::Path};

/// Marks the annotations this plugin writes, so a summary can find them.
pub const CAPTION_TYPE: &str = "caption_type";

pub fn provider(model: &str) -> String {
    format!("vlm:{model}")
}

/// Returns the annotations to append to `unit` and any warnings.
pub fn describe_unit(config: &Config, client: &Client, unit: &Value) -> (Vec<Value>, Vec<String>) {
    let mut warnings = Vec::new();
    let Some(path) = visual_path(unit) else {
        return (Vec::new(), warnings);
    };
    let provider = provider(&client.endpoint().model);
    if already_described(unit, &provider) {
        return (Vec::new(), warnings);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let jpeg = match encode_jpeg(path, config.max_side) {
        Ok(jpeg) => jpeg,
        Err(e) => {
            warnings.push(format!("vlm: {name}: {e:#}"));
            return (Vec::new(), warnings);
        }
    };
    let time_range = unit.get("time_range").cloned().unwrap_or(Value::Null);
    let engine = format!("{:?}", client.endpoint().engine).to_lowercase();
    let mut annotations = Vec::new();
    let stop = |e: CallError, what: &str, warnings: &mut Vec<String>| {
        warnings.push(format!("vlm: {name}: {what}: {e}"));
        e.unreachable
    };

    for prompt in &config.prompts {
        match client.describe(&jpeg, &prompt.kind, &prompt.text) {
            Ok(text) if text.is_empty() => {}
            Ok(text) => {
                let mut attributes = Map::new();
                attributes.insert(CAPTION_TYPE.into(), json!(prompt.kind));
                attributes.insert("prompt".into(), json!(prompt.text));
                attributes.insert("engine".into(), json!(engine));
                annotations.push(json!({
                    "kind": "caption",
                    "text": text,
                    "provider": provider,
                    "confidence": null,
                    "region": null,
                    "time_range": time_range,
                    "attributes": attributes,
                }));
            }
            Err(e) => {
                if stop(e, &prompt.kind, &mut warnings) {
                    return (annotations, warnings);
                }
            }
        }
    }

    for object in &config.detect {
        match client.detect(&jpeg, object) {
            Ok(boxes) => {
                for b in boxes {
                    let x = b.x_min.clamp(0.0, 1.0);
                    let y = b.y_min.clamp(0.0, 1.0);
                    let width = (b.x_max.clamp(0.0, 1.0) - x).max(0.0);
                    let height = (b.y_max.clamp(0.0, 1.0) - y).max(0.0);
                    if width == 0.0 || height == 0.0 {
                        continue;
                    }
                    annotations.push(json!({
                        "kind": "object",
                        "text": object,
                        "provider": provider,
                        "confidence": null,
                        "region": {"x": x, "y": y, "width": width, "height": height},
                        "time_range": time_range,
                        "attributes": {"detector": "open-vocabulary", "query": object},
                    }));
                }
            }
            Err(e) => {
                if stop(e, &format!("detect {object:?}"), &mut warnings) {
                    break;
                }
            }
        }
    }
    (annotations, warnings)
}

fn visual_path(unit: &Value) -> Option<&Path> {
    if unit["kind"] != "visual" {
        return None;
    }
    unit["visual_path"].as_str().map(Path::new)
}

fn already_described(unit: &Value, provider: &str) -> bool {
    unit["annotations"].as_array().is_some_and(|all| {
        all.iter().any(|a| {
            a["provider"] == provider
                && a["kind"] == "caption"
                && a["attributes"][CAPTION_TYPE].is_string()
        })
    })
}

/// Downscales to `max_side` and re-encodes as base64 JPEG.
pub fn encode_jpeg(path: &Path, max_side: u32) -> Result<String> {
    let image = image::open(path).with_context(|| format!("read image {}", path.display()))?;
    let image = if image.width() > max_side || image.height() > max_side {
        image.thumbnail(max_side, max_side)
    } else {
        image
    };
    let mut bytes = Vec::new();
    image
        .to_rgb8()
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
        .context("encode JPEG")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_images_are_downscaled_before_upload() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide.png");
        image::RgbImage::new(2000, 500).save(&path).unwrap();
        let encoded = encode_jpeg(&path, 400).unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (400, 100));
    }

    #[test]
    fn only_visual_units_with_an_image_are_described() {
        assert!(visual_path(&json!({"kind": "text", "visual_path": "/x.png"})).is_none());
        assert!(visual_path(&json!({"kind": "visual", "visual_path": null})).is_none());
        assert!(visual_path(&json!({"kind": "visual", "visual_path": "/x.png"})).is_some());
    }
}
