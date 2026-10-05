//! Graph handling on the plugin's side of the protocol: which sources need a
//! transcript, and how Whisper segments become a transcript unit.
//!
//! The graph stays a `serde_json::Value` so fields this plugin does not know
//! about survive the round trip unchanged.

use serde_json::{Map, Value, json};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MediaSource {
    pub id: String,
    pub path: PathBuf,
    pub name: String,
}

fn units(graph: &Value) -> impl Iterator<Item = &Value> {
    graph["units"].as_array().into_iter().flatten()
}

/// Audio and video sources that do not already carry a transcript (for
/// example from a sidecar `.srt` or `--transcript`).
pub fn media_sources(graph: &Value) -> Vec<MediaSource> {
    let Some(sources) = graph["sources"].as_array() else {
        return Vec::new();
    };
    sources
        .iter()
        .filter_map(|source| {
            let id = source["id"].as_str()?.to_string();
            let path = PathBuf::from(source["path"].as_str()?);
            let mime = source["detected_type"].as_str().unwrap_or("");
            let own_units: Vec<&Value> = units(graph)
                .filter(|u| u["source_id"].as_str() == Some(id.as_str()))
                .collect();
            let timed = own_units.iter().any(|u| {
                u["kind"] == "audio" || (u["kind"] == "visual" && !u["time_range"].is_null())
            });
            let media = mime.starts_with("audio/") || mime.starts_with("video/") || timed;
            let transcribed = own_units.iter().any(|u| {
                u["annotations"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|a| a["kind"] == "transcript"))
            });
            if !media || transcribed || own_units.is_empty() {
                return None;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "media".into());
            Some(MediaSource { id, path, name })
        })
        .collect()
}

/// Appends a visible transcript unit for `source` and points its audio
/// placeholder at it. Returns false when no segment has text.
pub fn apply(
    graph: &mut Value,
    source: &MediaSource,
    segments: &[Segment],
    provider: &str,
    language: Option<&str>,
) -> bool {
    let segments: Vec<Segment> = segments
        .iter()
        .filter(|s| !s.text.trim().is_empty() && s.start.is_finite() && s.end.is_finite())
        .map(|s| {
            let start = s.start.max(0.0);
            Segment {
                start,
                end: s.end.max(start),
                text: s.text.trim().to_string(),
            }
        })
        .collect();
    if segments.is_empty() {
        return false;
    }
    let Some(units) = graph["units"].as_array_mut() else {
        return false;
    };
    for unit in units.iter_mut() {
        if unit["source_id"].as_str() == Some(source.id.as_str()) && unit["kind"] == "audio" {
            unit["visible_text"] = json!(format!(
                "Audio source: {}\n\nTranscript follows.",
                source.name
            ));
        }
    }
    let mut attributes = Map::new();
    if let Some(language) = language.filter(|l| !l.is_empty()) {
        attributes.insert("language".into(), json!(language));
    }
    let text = segments
        .iter()
        .map(|s| {
            format!(
                "[{} --> {}] {}",
                timestamp(s.start),
                timestamp(s.end),
                s.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let annotations: Vec<Value> = segments
        .iter()
        .map(|s| {
            json!({
                "kind": "transcript",
                "text": s.text,
                "provider": provider,
                "confidence": null,
                "region": null,
                "time_range": {"start_seconds": s.start, "end_seconds": s.end},
                "attributes": attributes,
            })
        })
        .collect();
    units.push(json!({
        "id": uuid::Uuid::new_v4().to_string(),
        "source_id": source.id,
        "kind": "text",
        "visual_path": null,
        "visible_text": text,
        "time_range": null,
        "annotations": annotations,
        "metadata": {"transcript.engine": provider},
    }));
    true
}

fn timestamp(seconds: f64) -> String {
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        total_ms / 3_600_000,
        total_ms / 60_000 % 60,
        total_ms / 1000 % 60,
        total_ms % 1000
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> Value {
        json!({
            "sources": [
                {"id": "11111111-1111-4111-8111-111111111111", "path": "/media/talk.mp3",
                 "detected_type": "audio/mpeg", "metadata": {}},
                {"id": "22222222-2222-4222-8222-222222222222", "path": "/media/notes.txt",
                 "detected_type": null, "metadata": {}},
                {"id": "33333333-3333-4333-8333-333333333333", "path": "/media/clip.mp4",
                 "detected_type": "video/mp4", "metadata": {}}
            ],
            "units": [
                {"id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                 "source_id": "11111111-1111-4111-8111-111111111111", "kind": "audio",
                 "visual_path": null, "visible_text": "placeholder", "time_range": null,
                 "annotations": [], "metadata": {}, "future_field": 7},
                {"id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
                 "source_id": "22222222-2222-4222-8222-222222222222", "kind": "text",
                 "visual_path": null, "visible_text": "hi", "time_range": null,
                 "annotations": [], "metadata": {}},
                {"id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
                 "source_id": "33333333-3333-4333-8333-333333333333", "kind": "text",
                 "visual_path": null, "visible_text": "[00:00:00.000 --> 00:00:01.000] hi",
                 "time_range": null, "metadata": {},
                 "annotations": [{"kind": "transcript", "text": "hi", "provider": "sidecar",
                                  "confidence": null, "region": null,
                                  "time_range": {"start_seconds": 0.0, "end_seconds": 1.0},
                                  "attributes": {}}]}
            ],
            "metadata": {}
        })
    }

    #[test]
    fn only_untranscribed_media_sources_are_selected() {
        let found = media_sources(&graph());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "talk.mp3");
    }

    #[test]
    fn segments_become_a_timed_transcript_unit_and_keep_unknown_fields() {
        let mut graph = graph();
        let source = media_sources(&graph).remove(0);
        let segments = [
            Segment {
                start: 0.0,
                end: 2.5,
                text: " Hello there. ".into(),
            },
            Segment {
                start: 2.5,
                end: 2.0,
                text: "Clamped end".into(),
            },
            Segment {
                start: 3.0,
                end: 4.0,
                text: "   ".into(),
            },
        ];
        assert!(apply(
            &mut graph,
            &source,
            &segments,
            "whisper.cpp:ggml-base",
            Some("en")
        ));
        let units = graph["units"].as_array().unwrap();
        assert_eq!(units[0]["future_field"], 7);
        assert_eq!(
            units[0]["visible_text"],
            "Audio source: talk.mp3\n\nTranscript follows."
        );
        let added = units.last().unwrap();
        assert_eq!(added["source_id"], source.id);
        assert_eq!(
            added["visible_text"],
            "[00:00:00.000 --> 00:00:02.500] Hello there.\n[00:00:02.500 --> 00:00:02.500] Clamped end"
        );
        let annotations = added["annotations"].as_array().unwrap();
        assert_eq!(annotations.len(), 2);
        assert_eq!(annotations[1]["time_range"]["end_seconds"], 2.5);
        assert_eq!(annotations[0]["attributes"]["language"], "en");
    }

    #[test]
    fn silent_audio_adds_nothing() {
        let mut graph = graph();
        let before = graph.clone();
        let source = media_sources(&graph).remove(0);
        assert!(!apply(&mut graph, &source, &[], "whisper", None));
        assert_eq!(graph, before);
    }

    #[test]
    fn output_graph_deserializes_as_a_host_document_graph() {
        let mut graph = graph();
        let source = media_sources(&graph).remove(0);
        let segment = Segment {
            start: 1.0,
            end: 2.0,
            text: "wire check".into(),
        };
        apply(&mut graph, &source, &[segment], "whisper", None);
        let parsed: anytopdf_core::DocumentGraph = serde_json::from_value(graph).unwrap();
        parsed.validate().unwrap();
        let unit = parsed.units.last().unwrap();
        assert_eq!(unit.kind, anytopdf_core::UnitKind::Text);
        assert_eq!(
            unit.annotations[0].kind,
            anytopdf_core::AnnotationKind::Transcript
        );
    }
}
