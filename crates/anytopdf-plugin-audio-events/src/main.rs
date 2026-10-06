//! `anytopdf-plugin-audio-events`: an anytopdf runtime plugin (protocol v1)
//! that finds what can be heard in audio and video sources: speech, music,
//! noise and silence segments, raised voices (loudness well above the file's
//! own speech level), and, with an AudioSet model, events such as laughter,
//! applause, singing, sirens or a dog barking.
//!
//! It registers as a graph enricher, decodes each source's audio with FFmpeg
//! and adds one searchable "Audio events" text unit per source. It never
//! labels how a person feels.

mod dsp;
mod events;
mod tagger;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};
use tagger::{LabelMap, Tagger};

const PROTOCOL: u64 = 1;
const SIGNAL: &str = "audio-events:signal";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--anytopdf-manifest" => {
            println!("{}", manifest());
            ExitCode::SUCCESS
        }
        [flag] if flag == "--version" => {
            println!("anytopdf-plugin-audio-events {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [req_flag, request, resp_flag, response]
            if req_flag == "--anytopdf-request" && resp_flag == "--anytopdf-response" =>
        {
            let body = handle(Path::new(request)).unwrap_or_else(|e| {
                json!({"protocol": PROTOCOL, "ok": false, "warnings": [], "error": format!("{e:#}")})
            });
            match write_atomic(Path::new(response), &body) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("anytopdf-plugin-audio-events: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: anytopdf-plugin-audio-events --anytopdf-manifest\n       \
                 anytopdf-plugin-audio-events --anytopdf-request REQUEST --anytopdf-response RESPONSE"
            );
            ExitCode::from(2)
        }
    }
}

fn manifest() -> Value {
    json!({
        "protocol": PROTOCOL,
        "name": "audio-events",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": [{
            "kind": "graph-enricher",
            "extensions": [],
            "mime_types": ["audio/*", "video/*"],
            "priority": 40
        }]
    })
}

/// Settings from `ANYTOPDF_AUDIO_EVENTS_*`. Bad values fall back to the
/// default with a warning.
#[derive(Debug, Clone, PartialEq)]
struct Config {
    model: Option<PathBuf>,
    labels: Option<PathBuf>,
    device: String,
    threshold: f32,
    raised_lu: f32,
}

impl Config {
    fn from_env(warnings: &mut Vec<String>) -> Self {
        let var = |name: &str| env::var(name).ok().filter(|v| !v.trim().is_empty());
        let mut number = |name: &str, default: f32, range: std::ops::RangeInclusive<f32>| {
            let Some(raw) = var(name) else {
                return default;
            };
            match raw.trim().parse::<f32>() {
                Ok(v) if range.contains(&v) => v,
                _ => {
                    warnings.push(format!(
                        "audio-events: ignoring {name}={raw:?}; using {default}"
                    ));
                    default
                }
            }
        };
        let threshold = number("ANYTOPDF_AUDIO_EVENTS_THRESHOLD", 0.3, 0.0..=1.0);
        let raised_lu = number("ANYTOPDF_AUDIO_EVENTS_RAISED_LU", 8.0, 1.0..=40.0);
        Self {
            model: var("ANYTOPDF_AUDIO_EVENTS_MODEL").map(PathBuf::from),
            labels: var("ANYTOPDF_AUDIO_EVENTS_LABELS").map(PathBuf::from),
            device: var("ANYTOPDF_AUDIO_EVENTS_DEVICE")
                .unwrap_or_else(|| "cpu".into())
                .trim()
                .to_ascii_lowercase(),
            threshold,
            raised_lu,
        }
    }
}

/// A loaded model with the mapping from its classes to reported labels.
struct Model {
    tagger: Box<dyn Tagger>,
    labels: LabelMap,
    provider: String,
}

fn load_model(config: &Config) -> Result<Option<Model>> {
    let Some(path) = &config.model else {
        return Ok(None);
    };
    let labels_path = config
        .labels
        .clone()
        .or_else(|| tagger::default_labels(path))
        .context("no class names: set ANYTOPDF_AUDIO_EVENTS_LABELS to the model's class map CSV")?;
    let names = tagger::read_labels(&labels_path)?;
    let labels = LabelMap::new(&names);
    let tagger = tagger::OnnxTagger::load(path, names.len(), &config.device)?;
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".into());
    Ok(Some(Model {
        tagger: Box::new(tagger),
        labels,
        provider: format!("audio-events:{stem}"),
    }))
}

fn handle(request_path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(request_path).context("read request")?;
    let request: Value = serde_json::from_str(&raw).context("parse request")?;
    if request["protocol"].as_u64() != Some(PROTOCOL) {
        bail!("unsupported protocol {}", request["protocol"]);
    }
    if request["operation"] != "graph-enrich" {
        bail!("unsupported operation {}", request["operation"]);
    }
    let workspace = PathBuf::from(
        request["workspace"]
            .as_str()
            .context("request has no workspace")?,
    );
    let mut graph = request["graph"].clone();
    if !graph.is_object() {
        bail!("graph-enrich request has no graph");
    }

    let sources = events::media_sources(&graph);
    let mut response = json!({"protocol": PROTOCOL, "ok": true, "warnings": []});
    if sources.is_empty() {
        return Ok(response);
    }
    let mut warnings = Vec::new();
    let Ok(ffmpeg) = which::which("ffmpeg") else {
        warnings.push("audio-events: ffmpeg is required to read audio; nothing analyzed".into());
        response["warnings"] = json!(warnings);
        return Ok(response);
    };
    let config = Config::from_env(&mut warnings);
    let mut model = load_model(&config).unwrap_or_else(|e| {
        warnings.push(format!(
            "audio-events: sound-event model not used ({e:#}); reporting speech, music, silence and raised voices only"
        ));
        None
    });

    let mut changed = false;
    for source in &sources {
        let dir = workspace.join(format!("audio-events-{}", source.id));
        let result = fs::create_dir_all(&dir)
            .context("create work directory")
            .and_then(|()| {
                analyze_source(&ffmpeg, source, &dir, &config, &mut model, &mut warnings)
            });
        match result {
            Ok((found, engine)) => {
                // Audio too short for any event adds nothing, without a warning.
                changed |= events::apply(&mut graph, source, &found, &engine);
            }
            Err(e) => warnings.push(format!("audio-events: {}: {e:#}", source.name)),
        }
    }
    response["warnings"] = json!(warnings);
    if changed {
        response["graph"] = graph;
    }
    Ok(response)
}

fn analyze_source(
    ffmpeg: &Path,
    source: &events::MediaSource,
    dir: &Path,
    config: &Config,
    model: &mut Option<Model>,
    warnings: &mut Vec<String>,
) -> Result<(Vec<events::Event>, String)> {
    let log = dir.join("ffmpeg.log");
    let mut child = Command::new(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(&source.path)
        .args([
            "-map", "0:a:0", "-vn", "-ac", "1", "-ar", "16000", "-f", "f32le", "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(fs::File::create(&log).context("create ffmpeg log")?)
        .spawn()
        .context("run ffmpeg")?;
    let stdout = child.stdout.take().context("ffmpeg has no stdout")?;
    let analyzed = analyze(stdout, config, model, warnings);
    let status = child.wait().context("wait for ffmpeg")?;
    if !status.success() {
        let stderr = fs::read_to_string(&log).unwrap_or_default();
        if stderr.contains("matches no streams") {
            bail!("no audio track");
        }
        bail!(
            "ffmpeg could not decode audio: {}",
            stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
        );
    }
    analyzed
}

/// Reads 16 kHz mono `f32le` samples and returns the merged events plus the
/// engine name recorded on the unit.
fn analyze(
    mut input: impl Read,
    config: &Config,
    model: &mut Option<Model>,
    warnings: &mut Vec<String>,
) -> Result<(Vec<events::Event>, String)> {
    let mut analyzer = dsp::Analyzer::new();
    let mut windows = Vec::new();
    let mut bytes = vec![0u8; dsp::WINDOW * 4];
    let mut samples = Vec::with_capacity(dsp::WINDOW);
    let mut used_model = None;
    loop {
        let filled = read_full(&mut input, &mut bytes).context("read decoded audio")?;
        // Ignore a trailing partial window shorter than a quarter second.
        if filled * 4 < dsp::WINDOW {
            break;
        }
        samples.clear();
        samples.extend(
            bytes[..filled * 4]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        );
        let stats = analyzer.window(&samples);
        let mut scores = None;
        let mut found = Vec::new();
        let mut class_provider = SIGNAL.to_string();
        if stats.rms_dbfs >= dsp::SILENCE_DBFS
            && let Some(m) = model.as_mut()
        {
            samples.resize(dsp::WINDOW, 0.0);
            match m.tagger.scores(&samples) {
                Ok(s) => {
                    let (speech, music) = m.labels.speech_music(&s);
                    if !m.labels.speech.is_empty() && !m.labels.music.is_empty() {
                        scores = Some(dsp::ModelScores { speech, music });
                        class_provider = m.provider.clone();
                    }
                    found = m.labels.events(&s, config.threshold);
                    used_model = Some(m.provider.clone());
                }
                Err(e) => {
                    warnings.push(format!(
                        "audio-events: sound-event model failed ({e:#}); continuing without it"
                    ));
                    *model = None;
                }
            }
        }
        let class = dsp::classify(&stats, scores, config.threshold);
        windows.push(events::Window {
            stats,
            class,
            class_provider,
            events: found,
            raised_over: None,
        });
        if filled < dsp::WINDOW {
            break;
        }
    }
    let pairs: Vec<_> = windows.iter().map(|w| (w.stats, w.class)).collect();
    for (i, baseline) in dsp::raised_voices(&pairs, config.raised_lu) {
        windows[i].raised_over = Some(baseline);
    }
    let model_provider = used_model.clone().unwrap_or_default();
    let engine = match used_model {
        Some(p) => format!("signal+{}", p.trim_start_matches("audio-events:")),
        None => "signal".into(),
    };
    Ok((events::merge(&windows, &model_provider), engine))
}

/// Fills `buf` with whole samples; returns how many samples were read
/// (fewer than the buffer holds only at the end of the stream).
fn read_full(input: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match input.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled / 4)
}

fn write_atomic(path: &Path, body: &Value) -> Result<()> {
    let mut name = path
        .file_name()
        .context("response path has no file name")?
        .to_os_string();
    name.push(".tmp");
    let temporary = path.with_file_name(name);
    fs::write(&temporary, serde_json::to_vec(body)?).context("write response")?;
    fs::rename(&temporary, path).context("publish response")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsp::signals::*;

    fn config() -> Config {
        Config {
            model: None,
            labels: None,
            device: "cpu".into(),
            threshold: 0.3,
            raised_lu: 8.0,
        }
    }

    fn bytes(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    fn labels(events: &[events::Event]) -> Vec<(&str, f64, f64)> {
        events
            .iter()
            .map(|e| (e.label.as_str(), e.start, e.end))
            .collect()
    }

    /// Scores "laughter" while the window is loud and "Speech" otherwise.
    struct Mock {
        calls: usize,
        fail_after: Option<usize>,
    }

    impl Tagger for Mock {
        fn scores(&mut self, window: &[f32]) -> Result<Vec<f32>> {
            self.calls += 1;
            if self.fail_after.is_some_and(|n| self.calls > n) {
                bail!("device lost");
            }
            assert_eq!(window.len(), dsp::WINDOW);
            let peak = window.iter().fold(0f32, |a, b| a.max(b.abs()));
            Ok(if peak > 0.5 {
                vec![0.2, 0.1, 0.9]
            } else {
                vec![0.8, 0.1, 0.0]
            })
        }
    }

    fn mock_model(fail_after: Option<usize>) -> Option<Model> {
        Some(Model {
            tagger: Box::new(Mock {
                calls: 0,
                fail_after,
            }),
            labels: LabelMap::new(&["Speech".into(), "Music".into(), "Laughter".into()]),
            provider: "audio-events:mock".into(),
        })
    }

    #[test]
    fn signal_analysis_finds_speech_music_silence_and_a_raised_voice() {
        let mut audio = speech(6.0, 0.05);
        audio.extend(speech(2.0, 0.4));
        audio.extend(speech(2.0, 0.05));
        audio.extend(silence(4.0));
        audio.extend(tone(3.0, 440.0, 0.3));
        audio.extend(silence(0.1));
        let mut warnings = Vec::new();
        let (found, engine) = analyze(
            bytes(&audio).as_slice(),
            &config(),
            &mut None,
            &mut warnings,
        )
        .unwrap();
        assert_eq!(engine, "signal");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            labels(&found),
            vec![
                ("speech", 0.0, 10.0),
                ("raised-voice", 6.0, 8.0),
                ("silence", 10.0, 14.0),
                ("music", 14.0, 17.0),
            ]
        );
        assert!(found.iter().all(|e| e.provider == SIGNAL));
    }

    #[test]
    fn a_model_adds_events_and_decides_speech_versus_music() {
        let mut audio = tone(3.0, 440.0, 0.3);
        audio.extend(noise(2.0, 0.9, 3));
        audio.extend(silence(3.0));
        let mut model = mock_model(None);
        let mut warnings = Vec::new();
        let (found, engine) = analyze(
            bytes(&audio).as_slice(),
            &config(),
            &mut model,
            &mut warnings,
        )
        .unwrap();
        assert_eq!(engine, "signal+mock");
        assert_eq!(
            labels(&found),
            vec![
                ("speech", 0.0, 3.0),
                ("laughter", 3.0, 5.0),
                ("noise", 3.0, 5.0),
                ("silence", 5.0, 8.0),
            ]
        );
        assert_eq!(found[0].provider, "audio-events:mock");
        assert_eq!(found[1].confidence, Some(0.9));
        assert_eq!(found[2].provider, "audio-events:mock");
        assert_eq!(found[3].provider, SIGNAL);
    }

    #[test]
    fn a_failing_model_becomes_a_warning_and_signal_analysis_continues() {
        let audio = tone(4.0, 440.0, 0.3);
        let mut model = mock_model(Some(1));
        let mut warnings = Vec::new();
        let (found, _) = analyze(
            bytes(&audio).as_slice(),
            &config(),
            &mut model,
            &mut warnings,
        )
        .unwrap();
        assert!(model.is_none());
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("device lost"));
        assert_eq!(labels(&found), vec![("music", 1.0, 4.0)]);
    }

    #[test]
    fn audio_shorter_than_the_minimum_yields_no_events() {
        let audio = tone(0.2, 440.0, 0.3);
        let (found, _) = analyze(
            bytes(&audio).as_slice(),
            &config(),
            &mut None,
            &mut Vec::new(),
        )
        .unwrap();
        assert!(found.is_empty());
    }
}
