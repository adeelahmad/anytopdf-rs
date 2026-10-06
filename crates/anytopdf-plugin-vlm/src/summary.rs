//! `graph-enrich` (after unit enrichers): a summary page with a zero-shot
//! category and topics for each video, audio or transcript source, plus
//! per-scene summaries for videos, written from keyframe descriptions and any
//! transcript or subtitle text.

use crate::client::{CallError, Client, clean};
use crate::config::Config;
use crate::frames::CAPTION_TYPE;
use serde_json::{Value, json};
use std::collections::BTreeSet;

/// Keeps prompts within small local context windows.
const MAX_PROMPT_CHARS: usize = 12_000;

const MAX_TOPICS: usize = 5;

const GUARD: &str = "Describe only what is shown or said. Do not guess anyone's age, \
gender, ethnicity or emotions.";

/// One keyframe and its descriptions.
#[derive(Debug, Clone)]
struct Frame {
    unit_index: usize,
    seconds: f64,
    starts_scene: bool,
    descriptions: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Line {
    start: f64,
    end: f64,
    text: String,
}

/// Returns whether the graph changed, plus warnings.
pub fn summarize(config: &Config, client: &Client, graph: &mut Value) -> (bool, Vec<String>) {
    let mut warnings = Vec::new();
    let mut changed = false;
    let sources: Vec<(String, String, String)> = graph["sources"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let id = s["id"].as_str()?.to_string();
            let path = s["path"].as_str().unwrap_or_default();
            let name = std::path::Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| id.clone());
            let media = s["detected_type"].as_str().unwrap_or_default();
            Some((id, name, media.to_string()))
        })
        .collect();

    for (source_id, name, media) in sources {
        if already_summarized(graph, &source_id) {
            continue;
        }
        let video = media.starts_with("video/");
        let (frames, lines) = collect(graph, &source_id);
        // Videos summarize their keyframes; audio and subtitle files (and
        // anything else with timed speech) summarize the transcript alone.
        let evidence = if video {
            frames.iter().any(|f| !f.descriptions.is_empty()) || !lines.is_empty()
        } else {
            !lines.is_empty()
        };
        if !evidence {
            continue;
        }
        let frames = if video { frames } else { Vec::new() };
        let fail = |e: CallError, what: &str, warnings: &mut Vec<String>| {
            warnings.push(format!("vlm: {name}: {what}: {e}"));
            e.unreachable
        };

        let summary = match client.complete(&summary_prompt(video, &frames, &lines)) {
            Ok(s) if !s.is_empty() => clean(&s),
            Ok(_) => continue,
            Err(e) => {
                if fail(e, "summary", &mut warnings) {
                    break;
                }
                continue;
            }
        };
        let category = if config.categories.is_empty() {
            None
        } else {
            match client.complete(&category_prompt(&config.categories, &summary)) {
                Ok(answer) => pick_category(&config.categories, &answer),
                Err(e) => {
                    fail(e, "category", &mut warnings);
                    None
                }
            }
        };
        let topics = match client.complete(&topics_prompt(&summary)) {
            Ok(answer) => parse_topics(&answer),
            Err(e) => {
                fail(e, "topics", &mut warnings);
                Vec::new()
            }
        };
        // Scene summaries go on keyframes by index, so they come before the
        // summary page shifts the source's units.
        let scenes = scenes(&frames);
        let scenes = if scenes.len() < 2 {
            &[][..]
        } else {
            &scenes[..]
        };
        for (number, scene) in scenes.iter().enumerate() {
            let start = scene.first().map_or(0.0, |f| f.seconds);
            let end = scenes
                .get(number + 1)
                .and_then(|next| next.first())
                .map_or_else(|| scene.last().map_or(start, |f| f.seconds), |f| f.seconds);
            let scene_lines: Vec<&Line> = lines
                .iter()
                .filter(|l| l.end >= start && l.start <= end)
                .collect();
            if scene.iter().all(|f| f.descriptions.is_empty()) && scene_lines.is_empty() {
                continue;
            }
            match client.complete(&scene_prompt(scene, &scene_lines)) {
                Ok(text) if !text.is_empty() => {
                    let text = clean(&text);
                    let unit = &mut graph["units"][scene[0].unit_index];
                    if let Some(annotations) = unit["annotations"].as_array_mut() {
                        annotations.push(json!({
                            "kind": "caption",
                            "text": text,
                            "provider": crate::frames::provider(&client.endpoint().model),
                            "confidence": null,
                            "region": null,
                            "time_range": {"start_seconds": start, "end_seconds": end.max(start)},
                            "attributes": {CAPTION_TYPE: "scene-summary", "scene": (number + 1).to_string()},
                        }));
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    if fail(e, &format!("scene {} summary", number + 1), &mut warnings) {
                        break;
                    }
                }
            }
        }
        insert_summary_unit(
            graph,
            &source_id,
            &Summary {
                heading: if video { "Video summary" } else { "Summary" },
                text: &summary,
                category: category.as_deref(),
                topics: &topics,
                model: &client.endpoint().model,
            },
        );
        changed = true;
    }
    (changed, warnings)
}

fn already_summarized(graph: &Value, source_id: &str) -> bool {
    graph["units"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|u| u["source_id"] == source_id && u["metadata"]["vlm.summary"].is_string())
}

fn collect(graph: &Value, source_id: &str) -> (Vec<Frame>, Vec<Line>) {
    let mut frames = Vec::new();
    let mut seen = BTreeSet::new();
    let mut lines = Vec::new();
    for (unit_index, unit) in graph["units"].as_array().into_iter().flatten().enumerate() {
        if unit["source_id"] != source_id {
            continue;
        }
        let annotations = unit["annotations"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        for a in annotations {
            let ours = a["attributes"][CAPTION_TYPE].is_string();
            let spoken = a["kind"] == "transcript" || (a["kind"] == "caption" && !ours);
            let (Some(text), Some(start), Some(end)) = (
                a["text"].as_str(),
                a["time_range"]["start_seconds"].as_f64(),
                a["time_range"]["end_seconds"].as_f64(),
            ) else {
                continue;
            };
            // Sidecar captions copy cues onto every nearby keyframe.
            if spoken && seen.insert((start.to_bits(), end.to_bits(), text.to_string())) {
                lines.push(Line {
                    start,
                    end,
                    text: text.trim().to_string(),
                });
            }
        }
        if unit["kind"] != "visual" {
            continue;
        }
        let Some(seconds) = unit["time_range"]["start_seconds"].as_f64() else {
            continue;
        };
        let descriptions = annotations
            .iter()
            .filter(|a| {
                a["kind"] == "caption"
                    && a["attributes"][CAPTION_TYPE]
                        .as_str()
                        .is_some_and(|t| t != "scene-summary")
            })
            .filter_map(|a| a["text"].as_str().map(String::from))
            .collect();
        frames.push(Frame {
            unit_index,
            seconds,
            starts_scene: unit["metadata"]["video.frame-selection"] == "scene",
            descriptions,
        });
    }
    frames.sort_by(|a, b| a.seconds.total_cmp(&b.seconds));
    lines.sort_by(|a, b| a.start.total_cmp(&b.start));
    (frames, lines)
}

/// Splits keyframes at each FFmpeg scene-change frame.
fn scenes(frames: &[Frame]) -> Vec<Vec<Frame>> {
    let mut scenes: Vec<Vec<Frame>> = Vec::new();
    for frame in frames {
        match scenes.last_mut() {
            Some(scene) if !frame.starts_scene => scene.push(frame.clone()),
            _ => scenes.push(vec![frame.clone()]),
        }
    }
    scenes
}

fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        total / 60 % 60,
        total % 60
    )
}

fn evidence<'a>(
    frames: impl Iterator<Item = &'a Frame>,
    lines: impl Iterator<Item = &'a Line>,
) -> String {
    let mut out = String::from("Keyframe descriptions:\n");
    for frame in frames.filter(|f| !f.descriptions.is_empty()) {
        out.push_str(&format!(
            "[{}] {}\n",
            clock(frame.seconds),
            frame.descriptions.join(" ")
        ));
    }
    let mut lines = lines.peekable();
    if lines.peek().is_some() {
        out.push_str("\nTranscript:\n");
        for line in lines {
            out.push_str(&format!("[{}] {}\n", clock(line.start), line.text));
        }
    }
    truncate(out)
}

fn truncate(mut text: String) -> String {
    if text.len() > MAX_PROMPT_CHARS {
        let mut cut = MAX_PROMPT_CHARS;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[truncated]\n");
    }
    text
}

fn summary_prompt(video: bool, frames: &[Frame], lines: &[Line]) -> String {
    let what = if video {
        "Below are descriptions of keyframes from one video and any transcript. \
Write a short summary (three to five sentences) of what the video shows and says."
    } else {
        "Below is the transcript of one recording. \
Write a short summary (three to five sentences) of what it says."
    };
    format!(
        "{what} {GUARD}\n\n{}",
        evidence(frames.iter(), lines.iter())
    )
}

fn topics_prompt(summary: &str) -> String {
    format!(
        "List up to {MAX_TOPICS} short topics (one to three words each) covered by the content \
summarized below, one per line. Answer with the topics only.\n\nSummary: {summary}"
    )
}

/// Keeps up to five distinct, short topic lines, without list markers.
fn parse_topics(answer: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    answer
        .lines()
        .flat_map(|line| line.split(','))
        .map(|t| {
            t.trim()
                .trim_start_matches(|c: char| c.is_ascii_digit() || "-*•.)".contains(c))
                .trim()
                .trim_matches(|c: char| c == '"' || c == '.')
                .trim()
                .to_string()
        })
        .filter(|t| !t.is_empty() && t.chars().count() <= 60 && !t.ends_with(':'))
        .filter(|t| seen.insert(t.to_lowercase()))
        .take(MAX_TOPICS)
        .collect()
}

fn scene_prompt(scene: &[Frame], lines: &[&Line]) -> String {
    format!(
        "Below are descriptions of the keyframes of one scene from a video and any transcript \
for that scene. Summarize the scene in one or two sentences. {GUARD}\n\n{}",
        evidence(scene.iter(), lines.iter().copied())
    )
}

fn category_prompt(categories: &[String], summary: &str) -> String {
    format!(
        "Classify the content described below into exactly one of these categories: {}. \
Answer with the category name only.\n\nSummary: {summary}",
        categories.join(", ")
    )
}

/// The listed category the answer names first, ignoring case.
fn pick_category(categories: &[String], answer: &str) -> Option<String> {
    let answer = answer.to_lowercase();
    categories
        .iter()
        .filter_map(|c| answer.find(&c.to_lowercase()).map(|at| (at, c)))
        .min_by_key(|(at, _)| *at)
        .map(|(_, c)| c.clone())
}

struct Summary<'a> {
    heading: &'a str,
    text: &'a str,
    category: Option<&'a str>,
    topics: &'a [String],
    model: &'a str,
}

/// Inserts a visible summary page before the source's first unit.
fn insert_summary_unit(graph: &mut Value, source_id: &str, summary: &Summary) {
    let Some(units) = graph["units"].as_array_mut() else {
        return;
    };
    let position = units
        .iter()
        .position(|u| u["source_id"] == source_id)
        .unwrap_or(units.len());
    let provider = crate::frames::provider(summary.model);
    let entity = |text: &str, entity: &str| {
        json!({
            "kind": "custom",
            "text": text,
            "provider": provider,
            "confidence": null,
            "region": null,
            "time_range": null,
            "attributes": {"entity": entity},
        })
    };
    let mut text = format!("{}\n\n{}", summary.heading, summary.text);
    let mut annotations = Vec::new();
    if let Some(category) = summary.category {
        text.push_str(&format!("\n\nCategory: {category}"));
        annotations.push(entity(category, "category"));
    }
    if !summary.topics.is_empty() {
        text.push_str(&format!("\n\nTopics: {}", summary.topics.join(", ")));
        annotations.extend(summary.topics.iter().map(|t| entity(t, "topic")));
    }
    units.insert(
        position,
        json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "source_id": source_id,
            "kind": "text",
            "visual_path": null,
            "visible_text": text,
            "time_range": null,
            "annotations": annotations,
            "metadata": {"vlm.summary": "source", "vlm.model": summary.model},
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seconds: f64, starts_scene: bool) -> Frame {
        Frame {
            unit_index: 0,
            seconds,
            starts_scene,
            descriptions: vec![format!("frame at {seconds}")],
        }
    }

    #[test]
    fn scenes_split_at_scene_change_frames() {
        let frames = [
            frame(0.0, false),
            frame(5.0, false),
            frame(7.0, true),
            frame(12.0, false),
            frame(20.0, true),
        ];
        let sizes: Vec<usize> = scenes(&frames).iter().map(Vec::len).collect();
        assert_eq!(sizes, [2, 2, 1]);
    }

    #[test]
    fn topics_drop_list_markers_duplicates_and_extras() {
        let topics = parse_topics(
            "Topics:\n1. Fractions\n- whiteboard teaching\n* fractions\n\"Halves.\"\nA, B, C",
        );
        assert_eq!(
            topics,
            ["Fractions", "whiteboard teaching", "Halves", "A", "B"]
        );
        assert!(parse_topics("").is_empty());
    }

    #[test]
    fn category_is_the_first_listed_name_in_the_answer() {
        let categories: Vec<String> = ["news", "education", "sports"].map(String::from).into();
        assert_eq!(
            pick_category(&categories, "Education.").as_deref(),
            Some("education")
        );
        assert_eq!(
            pick_category(&categories, "sports news segment").as_deref(),
            Some("sports")
        );
        assert_eq!(pick_category(&categories, "cooking"), None);
    }

    #[test]
    fn prompts_carry_the_trait_guard_and_stay_bounded() {
        let long = Line {
            start: 0.0,
            end: 1.0,
            text: "é".repeat(MAX_PROMPT_CHARS),
        };
        let prompt = summary_prompt(true, &[frame(65.0, false)], &[long]);
        assert!(prompt.contains("Do not guess anyone's age"));
        assert!(prompt.contains("[00:01:05] frame at 65"));
        assert!(prompt.len() < MAX_PROMPT_CHARS + 1_000);
    }
}
