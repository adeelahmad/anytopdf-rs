//! Turns detected faces into protocol v1 `face` annotations.
//!
//! Each face becomes one annotation with text `face`, a normalized `region`,
//! the detector score as `confidence`, and these attributes:
//!
//! - `face_index`: 0-based position in this unit, highest score first
//! - `landmarks`: JSON `[[x, y], …]` of five normalized points in
//!   `landmark_order`
//! - `landmark_order`: `right_eye,left_eye,nose_tip,mouth_right,mouth_left`
//!   (the subject's right, which appears on the image's left)
//! - `crop`: optional workspace-relative path of a 112x112 aligned PNG crop
//! - `detector`: model name
//!
//! A unit with faces also gets one count annotation (`1 face`, `3 faces`) with
//! attribute `face_count` and no region. Both copy the unit's time range.

use crate::{align, detector::Face};
use anyhow::{Context, Result};
use image::RgbImage;
use serde_json::{Value, json};
use std::{fs, path::Path};

pub const PROVIDER: &str = "yunet";
pub const LANDMARK_ORDER: &str = "right_eye,left_eye,nose_tip,mouth_right,mouth_left";

/// True when this unit already carries this plugin's face annotations.
pub fn already_detected(unit: &Value) -> bool {
    unit["annotations"].as_array().is_some_and(|all| {
        all.iter()
            .any(|a| a["kind"] == "face" && a["provider"] == PROVIDER)
    })
}

/// Saves one aligned crop per face under `faces/<unit id>/` in the workspace
/// and returns their workspace-relative paths.
pub fn write_crops(
    workspace: &Path,
    unit: &Value,
    image: &RgbImage,
    faces: &[Face],
) -> Result<Vec<Option<String>>> {
    let id: String = unit["id"]
        .as_str()
        .unwrap_or("unit")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    let relative_dir = format!("faces/{}", if id.is_empty() { "unit" } else { &id });
    let dir = workspace.join(&relative_dir);
    fs::create_dir_all(&dir).context("create crop directory")?;
    faces
        .iter()
        .enumerate()
        .map(|(index, face)| {
            let Some(crop) = align::aligned_crop(image, &face.landmarks) else {
                return Ok(None);
            };
            let name = format!("face-{index}.png");
            crop.save(dir.join(&name))
                .with_context(|| format!("write {name}"))?;
            Ok(Some(format!("{relative_dir}/{name}")))
        })
        .collect()
}

pub fn annotations(
    unit: &Value,
    (width, height): (u32, u32),
    faces: &[Face],
    crops: &[Option<String>],
    model: &str,
) -> Vec<Value> {
    if faces.is_empty() {
        return Vec::new();
    }
    let (w, h) = (width.max(1) as f32, height.max(1) as f32);
    let time_range = unit.get("time_range").cloned().unwrap_or(Value::Null);
    let round = |v: f32| (f64::from(v.clamp(0.0, 1.0)) * 10_000.0).round() / 10_000.0;
    let mut out: Vec<Value> = faces
        .iter()
        .enumerate()
        .map(|(index, face)| {
            let (x0, y0) = ((face.x / w).clamp(0.0, 1.0), (face.y / h).clamp(0.0, 1.0));
            let x1 = ((face.x + face.width) / w).clamp(0.0, 1.0);
            let y1 = ((face.y + face.height) / h).clamp(0.0, 1.0);
            let landmarks: Vec<[f64; 2]> = face
                .landmarks
                .iter()
                .map(|(x, y)| [round(x / w), round(y / h)])
                .collect();
            let mut attributes = json!({
                "face_index": index.to_string(),
                "landmarks": serde_json::to_string(&landmarks).unwrap_or_default(),
                "landmark_order": LANDMARK_ORDER,
                "detector": model,
            });
            if let Some(Some(crop)) = crops.get(index) {
                attributes["crop"] = json!(crop);
            }
            json!({
                "kind": "face",
                "text": "face",
                "provider": PROVIDER,
                "confidence": round(face.score),
                "region": {
                    "x": round(x0), "y": round(y0),
                    "width": round(x1 - x0), "height": round(y1 - y0)
                },
                "time_range": time_range,
                "attributes": attributes,
            })
        })
        .collect();
    let count = faces.len();
    out.push(json!({
        "kind": "face",
        "text": format!("{count} face{}", if count == 1 { "" } else { "s" }),
        "provider": PROVIDER,
        "confidence": null,
        "region": null,
        "time_range": time_range,
        "attributes": {"face_count": count.to_string(), "detector": model},
    }));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x: f32, score: f32) -> Face {
        Face {
            x,
            y: 10.0,
            width: 20.0,
            height: 30.0,
            score,
            landmarks: [
                (x + 5.0, 20.0),
                (x + 15.0, 20.0),
                (x + 10.0, 28.0),
                (x + 6.0, 34.0),
                (x + 14.0, 34.0),
            ],
        }
    }

    #[test]
    fn faces_become_normalized_annotations_and_a_count() {
        let unit = json!({"id": "u", "kind": "visual",
                          "time_range": {"start_seconds": 4.0, "end_seconds": 4.0}});
        let found = annotations(
            &unit,
            (200, 100),
            &[face(10.0, 0.95), face(190.0, 0.85)],
            &[Some("faces/u/face-0.png".into()), None],
            "yunet-2026may",
        );
        assert_eq!(found.len(), 3);
        let first = &found[0];
        assert_eq!(first["kind"], "face");
        assert_eq!(first["text"], "face");
        assert_eq!(first["provider"], "yunet");
        assert_eq!(first["confidence"], 0.95);
        assert_eq!(
            first["region"],
            json!({"x": 0.05, "y": 0.1, "width": 0.1, "height": 0.3})
        );
        assert_eq!(first["time_range"]["start_seconds"], 4.0);
        assert_eq!(first["attributes"]["face_index"], "0");
        assert_eq!(first["attributes"]["crop"], "faces/u/face-0.png");
        assert_eq!(first["attributes"]["landmark_order"], LANDMARK_ORDER);
        let landmarks: Vec<[f64; 2]> =
            serde_json::from_str(first["attributes"]["landmarks"].as_str().unwrap()).unwrap();
        assert_eq!(landmarks[0], [0.075, 0.2]);
        // A box past the right edge is clipped inside the image.
        assert_eq!(found[1]["region"]["width"], 0.05);
        assert!(found[1]["attributes"].get("crop").is_none());
        assert_eq!(found[2]["text"], "2 faces");
        assert_eq!(found[2]["attributes"]["face_count"], "2");
        assert!(found[2]["region"].is_null());
    }

    #[test]
    fn no_faces_means_no_annotations() {
        assert!(annotations(&json!({}), (10, 10), &[], &[], "m").is_empty());
    }

    #[test]
    fn annotations_never_carry_inferred_traits() {
        let found = annotations(&json!({}), (100, 100), &[face(0.0, 0.9)], &[], "m");
        for a in &found {
            for key in a["attributes"].as_object().unwrap().keys() {
                for banned in ["age", "gender", "sex", "emotion", "race", "ethnicity"] {
                    assert!(!key.contains(banned), "attribute {key}");
                }
            }
        }
    }

    #[test]
    fn existing_yunet_faces_are_detected() {
        let unit = json!({"annotations": [{"kind": "face", "provider": "yunet"}]});
        assert!(already_detected(&unit));
        assert!(!already_detected(&json!({"annotations": []})));
    }

    #[test]
    fn crops_land_under_the_unit_directory() {
        let dir = tempfile::tempdir().unwrap();
        let image = RgbImage::from_pixel(200, 100, image::Rgb([90, 90, 90]));
        let unit = json!({"id": "../escape"});
        let crops = write_crops(dir.path(), &unit, &image, &[face(10.0, 0.9)]).unwrap();
        assert_eq!(crops, vec![Some("faces/escape/face-0.png".to_string())]);
        let crop = image::open(dir.path().join("faces/escape/face-0.png")).unwrap();
        assert_eq!((crop.width(), crop.height()), (112, 112));
    }
}
