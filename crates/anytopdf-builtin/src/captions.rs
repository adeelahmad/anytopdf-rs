use anyhow::{Result, bail};
use anytopdf_core::*;
use regex::Regex;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone)]
pub(crate) struct Cue {
    pub start: f64,
    pub end: f64,
    pub text: String,
    pub provider: String,
}

pub struct CaptionEnricher {
    explicit: Vec<PathBuf>,
    embedded: bool,
}

impl CaptionEnricher {
    pub fn new(explicit: Vec<PathBuf>, embedded: bool) -> Self {
        Self { explicit, embedded }
    }
}

impl Plugin for CaptionEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "captions-transcripts".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "graph-enricher".into(),
            extensions: vec![],
            mime_types: vec![],
            priority: 20,
        }
    }
}

impl GraphEnricher for CaptionEnricher {
    fn enrich_graph(&self, ctx: &JobContext, graph: &mut DocumentGraph) -> Result<Vec<String>> {
        let media: Vec<_> = graph
            .sources
            .iter()
            .filter(|source| {
                source
                    .detected_type
                    .as_deref()
                    .is_some_and(|m| m.starts_with("audio/") || m.starts_with("video/"))
                    || graph.units.iter().any(|u| {
                        u.source_id == source.id
                            && (u.kind == UnitKind::Audio
                                || (u.kind == UnitKind::Visual && u.time_range.is_some()))
                    })
            })
            .cloned()
            .collect();
        let mut warnings = Vec::new();
        let mut associated = BTreeSet::new();
        for source in &media {
            let mut files = sidecars(&source.path);
            let stem = source.path.file_stem();
            let same_stem = media.iter().filter(|m| m.path.file_stem() == stem).count();
            if media.len() == 1 {
                files.extend(self.explicit.iter().cloned());
            } else if same_stem == 1 {
                files.extend(
                    self.explicit
                        .iter()
                        .filter(|p| p.file_stem() == stem)
                        .cloned(),
                );
            }
            let mut seen = BTreeSet::new();
            let mut cues = Vec::new();
            for path in files {
                let path = match path.canonicalize() {
                    Ok(path) => path,
                    Err(e) => {
                        warnings.push(format!("caption {}: {e}", path.display()));
                        continue;
                    }
                };
                if !seen.insert(path.clone()) {
                    continue;
                }
                match parse_file(&path) {
                    Ok(mut parsed) => {
                        associated.insert(path);
                        cues.append(&mut parsed);
                    }
                    Err(e) => warnings.push(format!("caption {}: {e:#}", path.display())),
                }
            }
            if self.embedded
                && graph
                    .units
                    .iter()
                    .any(|u| u.source_id == source.id && u.kind == UnitKind::Visual)
            {
                match extract_embedded(&source.path, &ctx.workspace) {
                    Ok(mut parsed) => cues.append(&mut parsed),
                    Err(e) => warnings.push(format!(
                        "embedded captions {}: {e:#}",
                        source.path.display()
                    )),
                }
            }
            if cues.is_empty() {
                continue;
            }
            cues.sort_by(|a, b| {
                a.start
                    .total_cmp(&b.start)
                    .then_with(|| a.end.total_cmp(&b.end))
            });
            for unit in graph.units.iter_mut().filter(|u| u.source_id == source.id) {
                if unit.kind == UnitKind::Visual {
                    if let Some(ts) = unit.time_range.map(|t| t.start_seconds) {
                        unit.annotations.extend(
                            cues.iter()
                                .filter(|c| ts >= c.start - 0.25 && ts <= c.end + 0.25)
                                .map(cue_annotation),
                        );
                    }
                } else if unit.kind == UnitKind::Audio {
                    unit.visible_text = Some(format!(
                        "Audio source: {}\n\nTranscript follows.",
                        source.path.display()
                    ));
                }
            }
            graph.units.push(transcript_unit(source.id, &cues));
        }
        // Ambiguous or non-media transcripts retain their own source identity.
        for path in &self.explicit {
            let path = match path.canonicalize() {
                Ok(path) => path,
                Err(e) => {
                    warnings.push(format!("transcript {}: {e}", path.display()));
                    continue;
                }
            };
            if associated.contains(&path) || graph.sources.iter().any(|s| s.path == path) {
                continue;
            }
            if media.len() > 1 {
                warnings.push(
                    Diagnostic::new(
                        DiagnosticCode::TranscriptAmbiguous,
                        format!(
                            "transcript {} matches none of {} media sources by file name; kept as its own source",
                            path.display(),
                            media.len()
                        ),
                    )
                    .to_string(),
                );
            }
            match parse_file(&path) {
                Ok(cues) => {
                    let source = SourceRecord::new(path.clone());
                    graph.units.push(transcript_unit(source.id, &cues));
                    graph.sources.push(source);
                    associated.insert(path);
                }
                Err(e) => warnings.push(format!("transcript {}: {e:#}", path.display())),
            }
        }
        Ok(warnings)
    }
}

fn cue_annotation(c: &Cue) -> Annotation {
    Annotation {
        kind: AnnotationKind::Caption,
        text: c.text.clone(),
        provider: c.provider.clone(),
        confidence: None,
        region: None,
        time_range: Some(TimeRange {
            start_seconds: c.start,
            end_seconds: c.end,
        }),
        attributes: Metadata::new(),
    }
}

fn transcript_unit(source_id: uuid::Uuid, cues: &[Cue]) -> Unit {
    let text = cues
        .iter()
        .map(|c| format!("[{} --> {}] {}", fmt_ts(c.start), fmt_ts(c.end), c.text))
        .collect::<Vec<_>>()
        .join("\n");
    let mut unit = Unit::text(source_id, text);
    unit.annotations = cues
        .iter()
        .map(|cue| {
            let mut annotation = cue_annotation(cue);
            annotation.kind = AnnotationKind::Transcript;
            annotation
        })
        .collect();
    unit
}

fn sidecars(media: &Path) -> Vec<PathBuf> {
    ["srt", "vtt", "txt", "md"]
        .into_iter()
        .map(|ext| media.with_extension(ext))
        .filter(|p| p.is_file())
        .collect()
}

fn parse_file(path: &Path) -> Result<Vec<Cue>> {
    let raw = fs::read_to_string(path)?;
    let provider = format!("sidecar:{}", path.display());
    let ext = path
        .extension()
        .and_then(|x| x.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(ext.as_str(), "txt" | "md") {
        if raw.trim().is_empty() {
            bail!("empty transcript");
        }
        return Ok(vec![Cue {
            start: 0.0,
            end: f64::MAX / 2.0,
            text: raw,
            provider,
        }]);
    }
    parse_cues(&raw, &provider)
}

pub(crate) fn parse_cues(raw: &str, provider: &str) -> Result<Vec<Cue>> {
    let range = Regex::new(
        r"^\s*(?P<a>(?:\d{2,}:)?\d{2}:\d{2}[,.]\d{1,3})\s*-->\s*(?P<b>(?:\d{2,}:)?\d{2}:\d{2}[,.]\d{1,3})(?:\s.*)?$",
    )?;
    let tags = Regex::new(r"<[^>]+>")?;
    let lines: Vec<_> = raw.lines().collect();
    let mut cues = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(caps) = range.captures(lines[i]) {
            let start = parse_ts(&caps["a"])?;
            let end = parse_ts(&caps["b"])?;
            if end < start {
                bail!("subtitle ends before it starts");
            }
            i += 1;
            let mut payload = Vec::new();
            while i < lines.len() && !lines[i].trim().is_empty() {
                payload.push(lines[i].trim());
                i += 1;
            }
            let text = tags
                .replace_all(&payload.join(" "), "")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&nbsp;", " ")
                .replace("&amp;", "&");
            if !text.trim().is_empty() {
                cues.push(Cue {
                    start,
                    end,
                    text,
                    provider: provider.into(),
                });
            }
        } else if lines[i].contains("-->") {
            bail!("invalid subtitle timestamp at line {}", i + 1);
        }
        i += 1;
    }
    if cues.is_empty() {
        bail!("no subtitle cues found");
    }
    Ok(cues)
}

fn parse_ts(raw: &str) -> Result<f64> {
    let clean = raw.replace(',', ".");
    let parts: Vec<f64> = clean
        .split(':')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    let (hours, minutes, seconds) = match parts.as_slice() {
        [h, m, s] => (*h, *m, *s),
        [m, s] => (0.0, *m, *s),
        _ => bail!("invalid timestamp {raw}"),
    };
    if minutes >= 60.0 || seconds >= 60.0 {
        bail!("invalid timestamp {raw}");
    }
    Ok(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn fmt_ts(seconds: f64) -> String {
    if !seconds.is_finite() || seconds > 1.0e10 {
        return "end".into();
    }
    let total_ms = (seconds.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        total_ms / 3_600_000,
        total_ms / 60_000 % 60,
        total_ms / 1000 % 60,
        total_ms % 1000
    )
}

fn extract_embedded(media: &Path, workspace: &Path) -> Result<Vec<Cue>> {
    let ffmpeg = match which::which("ffmpeg") {
        Ok(v) => v,
        Err(_) => return Ok(vec![]),
    };
    let ffprobe = match which::which("ffprobe") {
        Ok(v) => v,
        Err(_) => return Ok(vec![]),
    };

    let probe = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-select_streams",
            "s",
            "-show_entries",
            "stream=index",
            "-of",
            "json",
        ])
        .arg(media)
        .bounded_output(std::time::Duration::from_secs(120))?;
    if !probe.status.success() {
        anyhow::bail!(
            "ffprobe subtitle discovery failed: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
    }

    let value: serde_json::Value = serde_json::from_slice(&probe.stdout)?;
    let indexes: Vec<i64> = value["streams"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["index"].as_i64())
        .collect();

    let mut out = Vec::new();
    for index in indexes {
        let target = workspace.join(format!("subtitle-{}-{index}.srt", uuid::Uuid::new_v4()));
        let result = Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-i"])
            .arg(media)
            .args(["-map", &format!("0:{index}"), "-c:s", "srt", "-y"])
            .arg(&target)
            .bounded_output(std::time::Duration::from_secs(120))?;
        if result.status.success() && target.exists() {
            let mut parsed = parse_file(&target)?;
            for cue in &mut parsed {
                cue.provider = format!("embedded:{}:stream:{index}", media.display());
            }
            out.extend(parsed);
        } else {
            anyhow::bail!("subtitle stream {index} could not be decoded to SRT");
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_srt_without_losing_numeric_dialogue() {
        let cues = parse_cues("1\r\n00:00:01,000 --> 00:00:02,500\r\n123\r\n\r\n2\r\n00:00:03,000 --> 00:00:04,000\r\n<b>Hello</b> &amp; welcome\r\n", "test").unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].text, "123");
        assert_eq!(cues[0].end, 2.5);
        assert_eq!(cues[1].text, "Hello & welcome");
    }

    #[test]
    fn parses_vtt_settings_and_ids() {
        let cues = parse_cues(
            "WEBVTT\n\ncue-id\n00:01.000 --> 00:02.000 align:start\nHello\n",
            "test",
        )
        .unwrap();
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].text, "Hello");
    }

    #[test]
    fn rejects_invalid_and_reversed_timestamps() {
        assert!(parse_cues("00:61.000 --> 00:62.000\nhello", "test").is_err());
        assert!(parse_cues("00:02.000 --> 00:01.000\nhello", "test").is_err());
        assert!(parse_cues("not subtitles", "test").is_err());
    }

    #[test]
    fn explicit_sidecar_is_not_duplicated_with_other_inputs() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("audio.srt");
        fs::write(&transcript, "1\n00:00:00,000 --> 00:00:01,000\nOnly once\n").unwrap();
        let mut audio = SourceRecord::new(dir.path().join("audio.wav"));
        audio.detected_type = Some("audio/wav".into());
        let text = SourceRecord::new(dir.path().join("other.txt"));
        let mut graph = DocumentGraph {
            sources: vec![audio, text],
            ..Default::default()
        };
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        CaptionEnricher::new(vec![transcript], false)
            .enrich_graph(&ctx, &mut graph)
            .unwrap();
        assert_eq!(graph.units.len(), 1);
        assert_eq!(graph.units[0].annotations.len(), 1);
    }

    #[test]
    fn ambiguous_transcript_gets_its_own_source() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("notes.txt");
        fs::write(&transcript, "Meeting notes").unwrap();
        let mut graph = DocumentGraph::default();
        for name in ["a.wav", "b.wav"] {
            let mut source = SourceRecord::new(dir.path().join(name));
            source.detected_type = Some("audio/wav".into());
            graph.sources.push(source);
        }
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        CaptionEnricher::new(vec![transcript.clone()], false)
            .enrich_graph(&ctx, &mut graph)
            .unwrap();
        assert_eq!(graph.sources.len(), 3);
        assert_eq!(
            graph.source(graph.units[0].source_id).unwrap().path,
            transcript.canonicalize().unwrap()
        );
    }
    fn two_audio_graph(dir: &Path) -> (DocumentGraph, uuid::Uuid) {
        let mut graph = DocumentGraph::default();
        let mut first_id = uuid::Uuid::nil();
        for name in ["a.mp3", "b.mp3"] {
            let path = dir.join(name);
            fs::write(&path, b"x").unwrap();
            let mut source = SourceRecord::new(path);
            source.detected_type = Some("audio/mpeg".into());
            if name == "a.mp3" {
                first_id = source.id;
            }
            let mut unit = Unit::text(source.id, String::new());
            unit.kind = UnitKind::Audio;
            unit.visible_text = None;
            graph.units.push(unit);
            graph.sources.push(source);
        }
        (graph, first_id)
    }

    #[test]
    fn transcript_with_several_media_sources_warns_ambiguous() {
        let media_dir = tempfile::tempdir().unwrap();
        let note_dir = tempfile::tempdir().unwrap();
        let notes = note_dir.path().join("notes.srt");
        fs::write(&notes, "1\n00:00:00,000 --> 00:00:01,000\nhello\n").unwrap();
        let (mut graph, _) = two_audio_graph(media_dir.path());
        let ctx = JobContext {
            workspace: media_dir.path().into(),
            quiet: true,
        };
        let warnings = CaptionEnricher::new(vec![notes.clone()], false)
            .enrich_graph(&ctx, &mut graph)
            .unwrap();
        let ambiguous: Vec<_> = warnings
            .iter()
            .map(|w| Diagnostic::from_wire(w))
            .filter(|d| d.code == DiagnosticCode::TranscriptAmbiguous)
            .collect();
        assert_eq!(ambiguous.len(), 1, "warnings: {warnings:?}");
        assert!(ambiguous[0].message.contains("notes.srt"));
        assert!(ambiguous[0].message.contains("2 media"));
        assert!(
            graph
                .sources
                .iter()
                .any(|s| s.path == notes.canonicalize().unwrap())
        );
    }

    #[test]
    fn transcript_matching_one_media_stem_is_associated() {
        let media_dir = tempfile::tempdir().unwrap();
        let note_dir = tempfile::tempdir().unwrap();
        let transcript = note_dir.path().join("a.srt");
        fs::write(
            &transcript,
            "1\n00:00:00,000 --> 00:00:01,000\nalpha line\n",
        )
        .unwrap();
        let (mut graph, a_id) = two_audio_graph(media_dir.path());
        let ctx = JobContext {
            workspace: media_dir.path().into(),
            quiet: true,
        };
        let warnings = CaptionEnricher::new(vec![transcript.clone()], false)
            .enrich_graph(&ctx, &mut graph)
            .unwrap();
        assert!(
            !warnings
                .iter()
                .any(|w| Diagnostic::from_wire(w).code == DiagnosticCode::TranscriptAmbiguous),
            "warnings: {warnings:?}"
        );
        assert!(graph.units.iter().any(|u| {
            u.source_id == a_id
                && u.visible_text
                    .as_deref()
                    .is_some_and(|t| t.contains("alpha line"))
        }));
        let canonical = transcript.canonicalize().unwrap();
        assert!(graph.sources.iter().all(|s| s.path != canonical));
    }
}
