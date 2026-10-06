//! Turns per-window results into timed events, and events into a searchable
//! "Audio events" text unit on the graph.
//!
//! The graph stays a `serde_json::Value` so fields this plugin does not know
//! about survive the round trip unchanged.

use crate::dsp::{Class, WindowStats};
use serde_json::{Map, Value, json};
use std::path::PathBuf;

pub const ENGINE_KEY: &str = "audio-events.engine";

/// What one window contributed: its speech/music/noise/silence class, which
/// provider decided it, model events, and the raised-voice baseline when the
/// window is well above the file's speech level.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub stats: WindowStats,
    pub class: Class,
    pub class_provider: String,
    pub events: Vec<(&'static str, f32)>,
    pub raised_over: Option<f32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub label: String,
    pub start: f64,
    pub end: f64,
    pub confidence: Option<f32>,
    pub provider: String,
    pub attributes: Map<String, Value>,
}

/// Labels that describe the whole soundtrack rather than a moment worth
/// putting on the timeline.
fn is_background(label: &str) -> bool {
    matches!(label, "speech" | "silence" | "noise")
}

fn min_seconds(label: &str) -> f64 {
    match label {
        "silence" => 3.0,
        "speech" | "music" | "noise" => 2.0,
        _ => 1.0,
    }
}

struct Open {
    first: usize,
    last: usize,
    confidence: Option<f32>,
    provider: String,
    loudness: f32,
    baseline: f32,
}

/// Merges runs of windows sharing a label (bridging one-window gaps) into
/// events, drops events shorter than the label's minimum, and sorts them by
/// start time.
pub fn merge(windows: &[Window], model_provider: &str) -> Vec<Event> {
    let mut labels: Vec<(String, usize, Option<f32>, String)> = Vec::new();
    for (i, w) in windows.iter().enumerate() {
        labels.push((w.class.label().into(), i, None, w.class_provider.clone()));
        for (label, score) in &w.events {
            labels.push(((*label).into(), i, Some(*score), model_provider.into()));
        }
        if w.raised_over.is_some() {
            labels.push(("raised-voice".into(), i, None, "audio-events:signal".into()));
        }
    }
    labels.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));

    let mut events = Vec::new();
    let mut open: Option<(String, Open)> = None;
    for (label, i, score, provider) in labels {
        let stats = &windows[i].stats;
        if let Some((current, run)) = &mut open
            && *current == label
            && i <= run.last + 2
        {
            run.last = i;
            run.confidence = max_option(run.confidence, score);
            run.loudness = run.loudness.max(stats.loudness);
            continue;
        }
        if let Some((current, run)) = open.take() {
            events.extend(close(&current, run, windows));
        }
        open = Some((
            label,
            Open {
                first: i,
                last: i,
                confidence: score,
                provider,
                loudness: stats.loudness,
                baseline: windows[i].raised_over.unwrap_or(f32::NAN),
            },
        ));
    }
    if let Some((current, run)) = open {
        events.extend(close(&current, run, windows));
    }
    events.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.label.cmp(&b.label)));
    events
}

fn max_option(a: Option<f32>, b: Option<f32>) -> Option<f32> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

fn close(label: &str, run: Open, windows: &[Window]) -> Option<Event> {
    let start = windows[run.first].stats.start;
    let end = windows[run.last].stats.end;
    if end - start < min_seconds(label) {
        return None;
    }
    let mut attributes = Map::new();
    attributes.insert("entity".into(), json!("audio-event"));
    attributes.insert("label".into(), json!(label));
    if label == "raised-voice" {
        attributes.insert(
            "loudness_lufs".into(),
            json!(format!("{:.1}", run.loudness)),
        );
        attributes.insert(
            "baseline_lufs".into(),
            json!(format!("{:.1}", run.baseline)),
        );
    }
    Some(Event {
        label: label.into(),
        start,
        end,
        confidence: run.confidence,
        provider: run.provider,
        attributes,
    })
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

/// Audio and video sources not yet analyzed by this plugin.
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
            let own: Vec<&Value> = units(graph)
                .filter(|u| u["source_id"].as_str() == Some(id.as_str()))
                .collect();
            let timed = own.iter().any(|u| {
                u["kind"] == "audio" || (u["kind"] == "visual" && !u["time_range"].is_null())
            });
            let media = mime.starts_with("audio/") || mime.starts_with("video/") || timed;
            let done = own.iter().any(|u| !u["metadata"][ENGINE_KEY].is_null());
            if !media || done || own.is_empty() {
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

/// Appends a visible "Audio events" unit for `source`: a compact timeline of
/// notable events, then every event with its time range. Each event is also
/// a `custom` annotation with `entity = audio-event`. Returns false when
/// there are no events.
pub fn apply(graph: &mut Value, source: &MediaSource, events: &[Event], engine: &str) -> bool {
    if events.is_empty() {
        return false;
    }
    let Some(units) = graph["units"].as_array_mut() else {
        return false;
    };
    let notable: Vec<String> = events
        .iter()
        .filter(|e| !is_background(&e.label))
        .map(|e| format!("{} {}", clock(e.start), e.label))
        .collect();
    let mut text = format!("Audio events: {}\n\nTimeline: ", source.name);
    if notable.is_empty() {
        text.push_str("only speech, noise or silence");
    } else {
        text.push_str(&notable.join(" · "));
    }
    text.push_str("\n\n");
    let lines: Vec<String> = events
        .iter()
        .map(|e| {
            let mut line = format!("[{} --> {}] {}", clock(e.start), clock(e.end), e.label);
            if let Some(c) = e.confidence {
                line.push_str(&format!(" ({:.0}%)", c * 100.0));
            }
            if let (Some(l), Some(b)) = (
                e.attributes.get("loudness_lufs").and_then(Value::as_str),
                e.attributes.get("baseline_lufs").and_then(Value::as_str),
            ) {
                line.push_str(&format!(" ({l} LUFS, speech level {b} LUFS)"));
            }
            line
        })
        .collect();
    text.push_str(&lines.join("\n"));

    let annotations: Vec<Value> = events
        .iter()
        .map(|e| {
            json!({
                "kind": "custom",
                "text": e.label,
                "provider": e.provider,
                "confidence": e.confidence,
                "region": null,
                "time_range": {"start_seconds": e.start, "end_seconds": e.end},
                "attributes": e.attributes,
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
        "metadata": {ENGINE_KEY: engine},
    }));
    true
}

/// `MM:SS`, or `H:MM:SS` from one hour on.
fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0).round() as u64;
    if total >= 3600 {
        format!("{}:{:02}:{:02}", total / 3600, total / 60 % 60, total % 60)
    } else {
        format!("{:02}:{:02}", total / 60, total % 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(i: usize, class: Class, events: Vec<(&'static str, f32)>) -> Window {
        Window {
            stats: WindowStats {
                start: i as f64,
                end: i as f64 + 1.0,
                rms_dbfs: -20.0,
                loudness: -23.0 + i as f32,
                low_energy_ratio: 0.0,
                high_zcr_ratio: 0.0,
                zcr: 0.0,
            },
            class,
            class_provider: "audio-events:signal".into(),
            events,
            raised_over: None,
        }
    }

    fn summary(events: &[Event]) -> Vec<(String, f64, f64)> {
        events
            .iter()
            .map(|e| (e.label.clone(), e.start, e.end))
            .collect()
    }

    #[test]
    fn runs_merge_across_one_window_gaps_and_short_runs_drop() {
        use Class::*;
        let classes = [
            Speech, Speech, Music, Speech, Speech, Silence, Silence, Music, Music, Music, Speech,
        ];
        let mut windows: Vec<Window> = classes
            .iter()
            .enumerate()
            .map(|(i, c)| window(i, *c, vec![]))
            .collect();
        windows[4].events.push(("laughter", 0.4));
        windows[6].events.push(("laughter", 0.7));
        windows[9].raised_over = Some(-30.0);
        let events = merge(&windows, "audio-events:yamnet");
        assert_eq!(
            summary(&events),
            vec![
                ("speech".into(), 0.0, 5.0),
                ("laughter".into(), 4.0, 7.0),
                ("music".into(), 7.0, 10.0),
                ("raised-voice".into(), 9.0, 10.0),
            ]
        );
        let laughter = &events[1];
        assert_eq!(laughter.confidence, Some(0.7));
        assert_eq!(laughter.provider, "audio-events:yamnet");
        assert_eq!(laughter.attributes["entity"], "audio-event");
        assert_eq!(events[3].attributes["baseline_lufs"], "-30.0");
        assert_eq!(events[3].attributes["loudness_lufs"], "-14.0");
    }

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
                 "visual_path": null, "visible_text": "Audio events: clip.mp4",
                 "time_range": null, "annotations": [],
                 "metadata": {"audio-events.engine": "signal"}}
            ],
            "metadata": {}
        })
    }

    fn event(label: &str, start: f64, end: f64, confidence: Option<f32>) -> Event {
        let mut attributes = Map::new();
        attributes.insert("entity".into(), json!("audio-event"));
        attributes.insert("label".into(), json!(label));
        Event {
            label: label.into(),
            start,
            end,
            confidence,
            provider: "audio-events:signal".into(),
            attributes,
        }
    }

    #[test]
    fn only_unanalyzed_media_sources_are_selected() {
        let found = media_sources(&graph());
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].name, "talk.mp3");
    }

    #[test]
    fn events_become_a_timeline_unit_and_keep_unknown_fields() {
        let mut graph = graph();
        let source = media_sources(&graph).remove(0);
        let events = [
            event("speech", 0.0, 12.0, None),
            event("applause", 12.0, 15.0, Some(0.82)),
            event("music", 220.0, 260.0, None),
            event("laughter", 3725.0, 3727.0, Some(0.5)),
        ];
        assert!(apply(&mut graph, &source, &events, "signal+yamnet"));
        let units = graph["units"].as_array().unwrap();
        assert_eq!(units[0]["future_field"], 7);
        let added = units.last().unwrap();
        assert_eq!(added["metadata"][ENGINE_KEY], "signal+yamnet");
        let text = added["visible_text"].as_str().unwrap();
        assert!(
            text.starts_with(
                "Audio events: talk.mp3\n\nTimeline: 00:12 applause · 03:40 music · 1:02:05 laughter\n\n"
            ),
            "{text}"
        );
        assert!(text.contains("[00:12 --> 00:15] applause (82%)"), "{text}");
        assert_eq!(added["annotations"][1]["kind"], "custom");
        assert_eq!(
            added["annotations"][1]["attributes"]["entity"],
            "audio-event"
        );
        assert!(media_sources(&graph).is_empty());
    }

    #[test]
    fn no_events_adds_nothing() {
        let mut graph = graph();
        let before = graph.clone();
        let source = media_sources(&graph).remove(0);
        assert!(!apply(&mut graph, &source, &[], "signal"));
        assert_eq!(graph, before);
    }

    #[test]
    fn output_graph_deserializes_as_a_host_document_graph() {
        let mut graph = graph();
        let source = media_sources(&graph).remove(0);
        apply(
            &mut graph,
            &source,
            &[event("silence", 1.0, 5.0, None)],
            "signal",
        );
        let parsed: anytopdf_core::DocumentGraph = serde_json::from_value(graph).unwrap();
        parsed.validate().unwrap();
        let unit = parsed.units.last().unwrap();
        assert_eq!(unit.kind, anytopdf_core::UnitKind::Text);
        let annotation = &unit.annotations[0];
        assert_eq!(annotation.kind, anytopdf_core::AnnotationKind::Custom);
        assert_eq!(annotation.attributes["label"], "silence");
        assert!(
            unit.visible_text
                .as_deref()
                .unwrap()
                .contains("Timeline: only speech, noise or silence")
        );
    }
}
