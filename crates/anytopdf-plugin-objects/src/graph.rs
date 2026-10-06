//! Graph handling on the plugin's side of the protocol: which units to look
//! at, and how detections become `object` annotations.
//!
//! The graph stays a `serde_json::Value` so fields this plugin does not know
//! about survive the round trip unchanged.

use crate::detector::Object;
use serde_json::{Map, Value, json};
use std::{collections::BTreeMap, path::PathBuf};

pub const PROVIDER: &str = "yolo";

/// A visual unit to run detection on.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub index: usize,
    pub path: PathBuf,
}

/// Visual units of image and video sources that this plugin has not already
/// annotated. Pages of documents (PDF, Office, HTML) are left alone.
pub fn targets(graph: &Value) -> Vec<Target> {
    let media: Vec<&str> = graph["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| {
            let mime = s["detected_type"].as_str().unwrap_or("");
            mime.starts_with("image/") || mime.starts_with("video/")
        })
        .filter_map(|s| s["id"].as_str())
        .collect();
    graph["units"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, unit)| {
            if unit["kind"] != "visual" {
                return None;
            }
            let path = unit["visual_path"].as_str()?;
            let timed = !unit["time_range"].is_null();
            let from_media = unit["source_id"]
                .as_str()
                .is_some_and(|id| media.contains(&id));
            let done = unit["annotations"]
                .as_array()
                .is_some_and(|a| a.iter().any(|a| a["provider"] == PROVIDER));
            ((from_media || timed) && !done).then(|| Target {
                index,
                path: PathBuf::from(path),
            })
        })
        .collect()
}

/// "3 person, 1 dog": counts by descending frequency, then by label.
pub fn summary(objects: &[Object]) -> Vec<(String, usize)> {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for object in objects {
        *counts.entry(object.label.as_str()).or_default() += 1;
    }
    let mut counts: Vec<(String, usize)> = counts
        .into_iter()
        .map(|(l, n)| (l.to_string(), n))
        .collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

/// Appends one annotation per object and one count summary to the unit.
/// Returns whether anything was added.
pub fn apply(graph: &mut Value, target: &Target, objects: &[Object], model: &str) -> bool {
    if objects.is_empty() {
        return false;
    }
    let Some(unit) = graph["units"].get_mut(target.index) else {
        return false;
    };
    let time_range = unit["time_range"].clone();
    let mut added: Vec<Value> = objects
        .iter()
        .map(|o| {
            let [x, y, width, height] = o.region;
            json!({
                "kind": "object",
                "text": o.label,
                "provider": PROVIDER,
                "confidence": o.confidence,
                "region": {"x": x, "y": y, "width": width, "height": height},
                "time_range": time_range,
                "attributes": {
                    "label": o.label,
                    "class_id": o.class_id.to_string(),
                    "model": model,
                }
            })
        })
        .collect();
    let counts = summary(objects);
    let text = counts
        .iter()
        .map(|(label, n)| format!("{n} {label}"))
        .collect::<Vec<_>>()
        .join(", ");
    added.push(json!({
        "kind": "object",
        "text": format!("objects: {text}"),
        "provider": PROVIDER,
        "confidence": null,
        "region": null,
        "time_range": time_range,
        "attributes": {
            "summary": "counts",
            "count": objects.len().to_string(),
            "model": model,
        }
    }));
    if !unit["annotations"].is_array() {
        unit["annotations"] = json!([]);
    }
    if let Some(annotations) = unit["annotations"].as_array_mut() {
        annotations.extend(added);
    }
    if !unit["metadata"].is_object() {
        unit["metadata"] = Value::Object(Map::new());
    }
    unit["metadata"]["objects.count"] = json!(objects.len().to_string());
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(label: &str) -> Object {
        Object {
            label: label.into(),
            class_id: 0,
            confidence: 0.8,
            region: [0.1, 0.2, 0.3, 0.4],
        }
    }

    fn graph() -> Value {
        json!({
            "sources": [
                {"id": "s-video", "path": "/in/a.mp4", "detected_type": "video/mp4"},
                {"id": "s-pdf", "path": "/in/b.pdf", "detected_type": "application/pdf"},
            ],
            "units": [
                {"id": "u0", "source_id": "s-video", "kind": "visual", "visual_path": "/w/f1.jpg",
                 "time_range": {"start_seconds": 5.0, "end_seconds": 5.0}, "annotations": [],
                 "metadata": {}, "future_field": 7},
                {"id": "u1", "source_id": "s-pdf", "kind": "visual", "visual_path": "/w/p1.png",
                 "time_range": null, "annotations": []},
                {"id": "u2", "source_id": "s-video", "kind": "audio", "visual_path": null},
                {"id": "u3", "source_id": "s-video", "kind": "visual", "visual_path": "/w/f2.jpg",
                 "annotations": [{"kind": "object", "provider": "yolo", "text": "cat"}]},
            ]
        })
    }

    #[test]
    fn targets_are_unannotated_media_frames_only() {
        let targets = targets(&graph());
        assert_eq!(
            targets,
            [Target {
                index: 0,
                path: PathBuf::from("/w/f1.jpg")
            }]
        );
    }

    #[test]
    fn apply_adds_located_objects_and_a_searchable_count() {
        let mut graph = graph();
        let target = targets(&graph).remove(0);
        let objects = [object("person"), object("dog"), object("person")];
        assert!(apply(&mut graph, &target, &objects, "yolo11n.onnx"));
        let unit = &graph["units"][0];
        let annotations = unit["annotations"].as_array().unwrap();
        assert_eq!(annotations.len(), 4);
        assert_eq!(annotations[0]["kind"], "object");
        assert_eq!(annotations[0]["text"], "person");
        assert_eq!(annotations[0]["provider"], "yolo");
        assert_eq!(annotations[0]["time_range"]["start_seconds"], 5.0);
        assert_eq!(annotations[0]["attributes"]["model"], "yolo11n.onnx");
        assert_eq!(annotations[3]["text"], "objects: 2 person, 1 dog");
        assert!(annotations[3]["region"].is_null());
        assert_eq!(unit["metadata"]["objects.count"], "3");
        assert_eq!(unit["future_field"], 7);
        assert!(!apply(&mut graph, &target, &[], "m"));
    }
}
