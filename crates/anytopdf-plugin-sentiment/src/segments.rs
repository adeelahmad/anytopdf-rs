//! Which text in a unit gets a sentiment: transcript segments, caption cues,
//! OCR blocks and the paragraphs of plain text units.
//!
//! The unit stays a `serde_json::Value`; this plugin only appends annotations,
//! so fields it does not know about are never rewritten.

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Transcript,
    Caption,
    Ocr,
    Text,
}

impl Origin {
    pub fn name(self) -> &'static str {
        match self {
            Origin::Transcript => "transcript",
            Origin::Caption => "caption",
            Origin::Ocr => "ocr",
            Origin::Text => "text",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.trim() {
            "transcript" => Some(Origin::Transcript),
            "caption" => Some(Origin::Caption),
            "ocr" => Some(Origin::Ocr),
            "text" => Some(Origin::Text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub text: String,
    pub from: Origin,
    pub time_range: Option<(f64, f64)>,
    pub region: Option<Region>,
    /// 1-based paragraph number for plain text units.
    pub paragraph: Option<usize>,
}

/// Plain text units can be whole books; past this many paragraphs the rest of
/// the unit is left unscored.
pub const MAX_TEXT_PARAGRAPHS: usize = 2000;

fn time_range(annotation: &Value) -> Option<(f64, f64)> {
    let t = &annotation["time_range"];
    Some((t["start_seconds"].as_f64()?, t["end_seconds"].as_f64()?))
}

fn region(annotation: &Value) -> Option<Region> {
    let r = &annotation["region"];
    Some(Region {
        x: r["x"].as_f64()?,
        y: r["y"].as_f64()?,
        width: r["width"].as_f64()?,
        height: r["height"].as_f64()?,
    })
}

/// The segments of `unit` drawn from the sources in `wanted`, in unit order.
pub fn collect(unit: &Value, wanted: &[Origin]) -> Vec<Segment> {
    let annotations: Vec<&Value> = unit["annotations"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let mut segments: Vec<Segment> = Vec::new();
    let mut push = |segment: Segment| {
        if !segment.text.trim().is_empty() && !segments.contains(&segment) {
            segments.push(segment);
        }
    };

    for annotation in &annotations {
        let from = match annotation["kind"].as_str() {
            Some("transcript") => Origin::Transcript,
            Some("caption") => Origin::Caption,
            _ => continue,
        };
        if wanted.contains(&from) {
            push(Segment {
                text: annotation["text"].as_str().unwrap_or("").trim().to_string(),
                from,
                time_range: time_range(annotation),
                region: None,
                paragraph: None,
            });
        }
    }

    if wanted.contains(&Origin::Ocr) {
        let ocr: Vec<&Value> = annotations
            .iter()
            .copied()
            .filter(|a| a["kind"] == "ocr")
            .collect();
        let unit_time = time_range(unit);
        for block in ocr_blocks(&ocr) {
            push(Segment {
                time_range: unit_time,
                ..block
            });
        }
    }

    let timed_text = annotations
        .iter()
        .any(|a| a["kind"] == "transcript" || a["kind"] == "caption");
    if wanted.contains(&Origin::Text) && unit["kind"] == "text" && !timed_text {
        let text = unit["visible_text"].as_str().unwrap_or("");
        for (n, paragraph) in paragraphs(text).take(MAX_TEXT_PARAGRAPHS).enumerate() {
            push(Segment {
                text: paragraph,
                from: Origin::Text,
                time_range: None,
                region: None,
                paragraph: Some(n + 1),
            });
        }
    }
    segments
}

/// Paragraphs separated by blank lines, with their lines joined by spaces.
fn paragraphs(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split("\n\n")
        .flat_map(|chunk| chunk.split("\r\n\r\n"))
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| p.chars().any(char::is_alphabetic))
}

struct Word<'a> {
    text: &'a str,
    region: Region,
}

/// Groups positioned OCR words (or lines) into lines by vertical overlap and
/// lines into blocks by vertical gap, so a sentence split over several OCR
/// words is scored as one. OCR text without a position is its own segment.
fn ocr_blocks(ocr: &[&Value]) -> Vec<Segment> {
    let mut out = Vec::new();
    let mut words = Vec::new();
    for annotation in ocr {
        let text = annotation["text"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        match region(annotation).filter(|r| r.height > 0.0 && r.width >= 0.0) {
            Some(region) => words.push(Word { text, region }),
            None => out.push(Segment {
                text: text.to_string(),
                from: Origin::Ocr,
                time_range: None,
                region: None,
                paragraph: None,
            }),
        }
    }
    let center = |r: &Region| r.y + r.height / 2.0;
    words.sort_by(|a, b| {
        center(&a.region)
            .total_cmp(&center(&b.region))
            .then(a.region.x.total_cmp(&b.region.x))
    });

    // Lines: a word joins the current line when its centre lies within half
    // a line height of the line's centre.
    let mut lines: Vec<Vec<Word>> = Vec::new();
    for word in words {
        match lines.last_mut() {
            Some(line)
                if {
                    let bounds = union(line.iter().map(|w| &w.region));
                    (center(&word.region) - center(&bounds)).abs()
                        < bounds.height.max(word.region.height) / 2.0
                } =>
            {
                line.push(word)
            }
            _ => lines.push(vec![word]),
        }
    }
    for line in &mut lines {
        line.sort_by(|a, b| a.region.x.total_cmp(&b.region.x));
    }

    // Blocks: consecutive lines closer than one line height.
    let mut blocks: Vec<(Vec<String>, Region)> = Vec::new();
    for line in lines {
        let text = line.iter().map(|w| w.text).collect::<Vec<_>>().join(" ");
        let bounds = union(line.iter().map(|w| &w.region));
        match blocks.last_mut() {
            Some((texts, block))
                if bounds.y - (block.y + block.height) < bounds.height.max(1e-6) =>
            {
                texts.push(text);
                *block = union([&*block, &bounds]);
            }
            _ => blocks.push((vec![text], bounds)),
        }
    }
    out.extend(blocks.into_iter().map(|(texts, region)| Segment {
        text: texts.join(" "),
        from: Origin::Ocr,
        time_range: None,
        region: Some(region),
        paragraph: None,
    }));
    out
}

fn union<'a>(regions: impl IntoIterator<Item = &'a Region>) -> Region {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for r in regions {
        x0 = x0.min(r.x);
        y0 = y0.min(r.y);
        x1 = x1.max(r.x + r.width);
        y1 = y1.max(r.y + r.height);
    }
    Region {
        x: x0,
        y: y0,
        width: (x1 - x0).max(0.0),
        height: (y1 - y0).max(0.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ALL: &[Origin] = &[
        Origin::Transcript,
        Origin::Caption,
        Origin::Ocr,
        Origin::Text,
    ];

    fn word(text: &str, x: f64, y: f64) -> Value {
        json!({"kind": "ocr", "text": text, "provider": "tesseract",
               "region": {"x": x, "y": y, "width": 0.08, "height": 0.03}})
    }

    #[test]
    fn ocr_words_are_grouped_into_lines_and_blocks() {
        let unit = json!({
            "kind": "visual",
            "time_range": {"start_seconds": 12.0, "end_seconds": 12.0},
            "annotations": [
                word("terrible", 0.30, 0.101), word("This", 0.10, 0.10), word("is", 0.20, 0.102),
                word("service", 0.10, 0.14),
                word("Footer", 0.10, 0.80),
                {"kind": "ocr", "text": "loose text", "provider": "doctr"},
                {"kind": "object", "text": "cup", "provider": "yolo"}
            ]
        });
        let segments = collect(&unit, ALL);
        let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, ["loose text", "This is terrible service", "Footer"]);
        let block = &segments[1];
        assert_eq!(block.from, Origin::Ocr);
        assert_eq!(block.time_range, Some((12.0, 12.0)));
        let region = block.region.as_ref().unwrap();
        assert!((region.x - 0.10).abs() < 1e-9 && (region.y - 0.10).abs() < 1e-9);
        assert!((region.x + region.width - 0.38).abs() < 1e-9);
        assert!((region.y + region.height - 0.17).abs() < 1e-9);
    }

    #[test]
    fn transcripts_and_captions_keep_their_times_and_repeats_are_dropped() {
        let cue = |kind: &str, text: &str, start: f64| {
            json!({"kind": kind, "text": text, "provider": "whisper.cpp",
                   "time_range": {"start_seconds": start, "end_seconds": start + 2.0}})
        };
        let unit = json!({
            "kind": "text",
            "visible_text": "[00:00:01.000 --> 00:00:03.000] I love it",
            "annotations": [cue("transcript", "I love it", 1.0), cue("transcript", "I love it", 1.0),
                            cue("caption", "Awful.", 5.0), cue("transcript", "  ", 7.0)]
        });
        let segments = collect(&unit, ALL);
        assert_eq!(segments.len(), 2, "{segments:?}");
        assert_eq!(segments[0].from, Origin::Transcript);
        assert_eq!(segments[0].time_range, Some((1.0, 3.0)));
        assert_eq!(segments[1].from, Origin::Caption);
        // The transcript's visible text is not scored a second time.
        assert!(segments.iter().all(|s| s.from != Origin::Text));
        assert!(
            collect(&unit, &[Origin::Caption])
                .iter()
                .all(|s| s.from == Origin::Caption)
        );
    }

    #[test]
    fn plain_text_units_are_scored_by_paragraph() {
        let unit = json!({
            "kind": "text",
            "visible_text": "Dear team,\nthe update is great.\n\n----\n\nIt broke my laptop.",
            "annotations": []
        });
        let segments = collect(&unit, ALL);
        let got: Vec<(&str, Option<usize>)> = segments
            .iter()
            .map(|s| (s.text.as_str(), s.paragraph))
            .collect();
        assert_eq!(
            got,
            [
                ("Dear team, the update is great.", Some(1)),
                ("It broke my laptop.", Some(2))
            ]
        );
        assert!(collect(&unit, &[Origin::Ocr]).is_empty());
    }
}
