//! Whisper engines this plugin can drive, and how each one's JSON output
//! becomes segments.

use crate::transcript::Segment;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// whisper.cpp's `whisper-cli` with a ggml model file.
    WhisperCpp,
    /// The OpenAI `whisper` CLI or a flag-compatible one such as
    /// `whisper-ctranslate2` (faster-whisper).
    OpenAi,
}

#[derive(Debug, Clone)]
pub struct Backend {
    pub engine: Engine,
    pub executable: PathBuf,
    pub model: String,
    pub language: Option<String>,
}

/// Plugin settings, read from `ANYTOPDF_WHISPER_*` environment variables.
#[derive(Debug, Clone, Default)]
pub struct Config {
    pub engine: Option<String>,
    pub executable: Option<PathBuf>,
    pub model: Option<String>,
    pub language: Option<String>,
    /// ggml model installed by `anytopdf setup whisper`; whisper.cpp uses it
    /// when `ANYTOPDF_WHISPER_MODEL` does not name a model file.
    pub recorded_model: Option<PathBuf>,
}

impl Config {
    pub fn from_env() -> Self {
        let var = |name: &str| env::var(name).ok().filter(|v| !v.trim().is_empty());
        Self {
            engine: var("ANYTOPDF_WHISPER_ENGINE"),
            executable: var("ANYTOPDF_WHISPER_BIN").map(PathBuf::from),
            model: var("ANYTOPDF_WHISPER_MODEL"),
            language: var("ANYTOPDF_WHISPER_LANGUAGE").filter(|l| l != "auto"),
            recorded_model: crate::setup::recorded_model(),
        }
    }
}

/// How to get an engine on this platform.
pub fn engine_hint() -> &'static str {
    if cfg!(target_os = "macos") {
        "install whisper.cpp with `brew install whisper-cpp`"
    } else if cfg!(windows) {
        "install whisper.cpp by unpacking whisper-bin-x64.zip from \
         https://github.com/ggml-org/whisper.cpp/releases and putting whisper-cli.exe on PATH, \
         or run `pipx install whisper-ctranslate2`"
    } else {
        "install whisper.cpp (`brew install whisper-cpp`, or build whisper-cli from \
         https://github.com/ggml-org/whisper.cpp) or run `pipx install whisper-ctranslate2`"
    }
}

const SETUP_HINT: &str = "run `anytopdf setup whisper` to download a model";

/// The shortest way to turn transcription on here when no engine is installed:
/// one command, then the alternative. faster-whisper fetches its own model, so it
/// needs no `setup whisper`.
pub fn setup_steps(has_model: bool) -> String {
    let setup = if has_model {
        ""
    } else {
        " && anytopdf setup whisper"
    };
    let pipx = "`pipx install whisper-ctranslate2` (faster-whisper; it downloads its \
                own model on first use)";
    if cfg!(target_os = "macos") {
        format!("run `brew install whisper-cpp{setup}`, or {pipx}")
    } else {
        let cpp = if cfg!(windows) {
            "unpack whisper-bin-x64.zip from https://github.com/ggml-org/whisper.cpp/releases \
             and put whisper-cli.exe on PATH"
        } else {
            "build whisper-cli from https://github.com/ggml-org/whisper.cpp and put it on PATH"
        };
        let then = if has_model {
            String::new()
        } else {
            format!(", then {SETUP_HINT}")
        };
        format!("run {pipx}, or {cpp}{then}")
    }
}

const CPP_NAMES: [&str; 2] = ["whisper-cli", "whisper-cpp"];
const OPENAI_NAMES: [&str; 2] = ["whisper-ctranslate2", "whisper"];
const OPENAI_DEFAULT_MODEL: &str = "base";

/// Picks an engine: an explicit `ANYTOPDF_WHISPER_ENGINE`/`_BIN` first, then
/// whisper.cpp when a model file is configured, then the OpenAI-compatible CLIs.
/// The error explains what is missing.
pub fn detect(config: &Config, lookup: impl Fn(&str) -> Option<PathBuf>) -> Result<Backend> {
    let engine = match config.engine.as_deref() {
        None | Some("auto") => None,
        Some("whisper.cpp" | "whisper-cpp" | "cpp") => Some(Engine::WhisperCpp),
        Some("openai" | "openai-whisper" | "faster-whisper") => Some(Engine::OpenAi),
        Some(other) => bail!(
            "unknown ANYTOPDF_WHISPER_ENGINE {other:?}; use auto, whisper.cpp or openai-whisper"
        ),
    };
    let engine = engine.or_else(|| {
        config.executable.as_deref().map(|bin| {
            let stem = bin
                .file_stem()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if OPENAI_NAMES.contains(&stem.as_str()) {
                Engine::OpenAi
            } else {
                Engine::WhisperCpp
            }
        })
    });
    let find = |names: &[&str]| -> Option<PathBuf> {
        config
            .executable
            .clone()
            .or_else(|| names.iter().find_map(|n| lookup(n)))
    };
    let recorded = config
        .recorded_model
        .as_deref()
        .and_then(|path| path.to_str());
    let cpp_model = config
        .model
        .as_deref()
        .filter(|m| Path::new(m).is_file())
        .or(recorded);
    let backend = |engine, executable, model: &str| Backend {
        engine,
        executable,
        model: model.to_string(),
        language: config.language.clone(),
    };

    match engine {
        Some(Engine::WhisperCpp) => {
            let bin = find(&CPP_NAMES).with_context(|| {
                format!(
                    "whisper.cpp (whisper-cli) not found on PATH; {}",
                    engine_hint()
                )
            })?;
            let model = cpp_model.with_context(|| {
                format!(
                    "whisper.cpp has no ggml model; {SETUP_HINT}, \
                     or set ANYTOPDF_WHISPER_MODEL to a ggml model file"
                )
            })?;
            Ok(backend(Engine::WhisperCpp, bin, model))
        }
        Some(Engine::OpenAi) => {
            let bin =
                find(&OPENAI_NAMES).context("whisper or whisper-ctranslate2 not found on PATH")?;
            Ok(backend(Engine::OpenAi, bin, &openai_model(config)))
        }
        None => {
            let cpp = find(&CPP_NAMES);
            if let (Some(bin), Some(model)) = (cpp.clone(), cpp_model) {
                return Ok(backend(Engine::WhisperCpp, bin, model));
            }
            if let Some(bin) = OPENAI_NAMES.iter().find_map(|n| lookup(n)) {
                return Ok(backend(Engine::OpenAi, bin, &openai_model(config)));
            }
            if cpp.is_some() {
                bail!(
                    "whisper.cpp found but has no ggml model; {SETUP_HINT}, \
                     or set ANYTOPDF_WHISPER_MODEL to a ggml model file"
                );
            }
            bail!(
                "no Whisper engine found; {}",
                setup_steps(cpp_model.is_some())
            )
        }
    }
}

/// Whether the plugin can transcribe here, and what it found or still needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    pub ready: bool,
    pub detail: String,
}

/// Checks the engine, the model and FFmpeg without running anything.
pub fn readiness(config: &Config, lookup: impl Fn(&str) -> Option<PathBuf>) -> Readiness {
    match detect(config, &lookup) {
        Err(e) => Readiness {
            ready: false,
            detail: format!("{e:#}"),
        },
        Ok(_) if lookup("ffmpeg").is_none() => Readiness {
            ready: false,
            detail: "ffmpeg is required to read audio; install FFmpeg and put it on PATH".into(),
        },
        Ok(backend) => Readiness {
            ready: true,
            detail: format!(
                "{} via {}",
                backend.provider(),
                backend.executable.display()
            ),
        },
    }
}

fn openai_model(config: &Config) -> String {
    config
        .model
        .clone()
        .unwrap_or_else(|| OPENAI_DEFAULT_MODEL.into())
}

impl Backend {
    /// Provider label recorded on every transcript annotation.
    pub fn provider(&self) -> String {
        let model = Path::new(&self.model)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.model.clone());
        let engine = match self.engine {
            Engine::WhisperCpp => "whisper.cpp".to_string(),
            Engine::OpenAi => self
                .executable
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "whisper".into()),
        };
        format!("{engine}:{model}")
    }

    /// Transcribes a 16 kHz mono WAV file, writing engine output into `dir`.
    pub fn transcribe(&self, wav: &Path, dir: &Path) -> Result<(Vec<Segment>, Option<String>)> {
        let mut command = Command::new(&self.executable);
        let output_json = match self.engine {
            Engine::WhisperCpp => {
                let prefix = dir.join("transcript");
                command
                    .arg("-m")
                    .arg(&self.model)
                    .arg("-f")
                    .arg(wav)
                    .args(["-l", self.language.as_deref().unwrap_or("auto")])
                    .args(["-oj", "-np", "-of"])
                    .arg(&prefix);
                prefix.with_extension("json")
            }
            Engine::OpenAi => {
                command
                    .arg(wav)
                    .args(["--model", &self.model])
                    .args(["--output_format", "json", "--verbose", "False"])
                    .arg("--output_dir")
                    .arg(dir);
                if let Some(language) = &self.language {
                    command.args(["--language", language]);
                }
                let stem = wav.file_stem().unwrap_or_default();
                dir.join(stem).with_extension("json")
            }
        };
        let output = command
            .stdin(std::process::Stdio::null())
            .output()
            .with_context(|| format!("run {}", self.executable.display()))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "{} failed: {}",
                self.executable.display(),
                stderr
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
            );
        }
        let raw = fs::read_to_string(&output_json)
            .with_context(|| format!("read {}", output_json.display()))?;
        let json: Value = serde_json::from_str(&raw).context("parse Whisper JSON output")?;
        match self.engine {
            Engine::WhisperCpp => parse_whisper_cpp(&json),
            Engine::OpenAi => parse_openai(&json),
        }
    }
}

/// whisper.cpp `-oj`: `transcription[].offsets.{from,to}` in milliseconds.
pub fn parse_whisper_cpp(json: &Value) -> Result<(Vec<Segment>, Option<String>)> {
    let items = json["transcription"]
        .as_array()
        .context("whisper.cpp output has no transcription array")?;
    let segments = items
        .iter()
        .map(|item| {
            let ms = |key: &str| item["offsets"][key].as_f64().map(|v| v / 1000.0);
            Ok(Segment {
                start: ms("from").context("segment without offsets.from")?,
                end: ms("to").context("segment without offsets.to")?,
                text: item["text"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect::<Result<_>>()?;
    let language = json["result"]["language"].as_str().map(str::to_string);
    Ok((segments, language))
}

/// OpenAI whisper / whisper-ctranslate2 JSON: `segments[].{start,end,text}` in seconds.
pub fn parse_openai(json: &Value) -> Result<(Vec<Segment>, Option<String>)> {
    let items = json["segments"]
        .as_array()
        .context("Whisper output has no segments array")?;
    let segments = items
        .iter()
        .map(|item| {
            Ok(Segment {
                start: item["start"].as_f64().context("segment without start")?,
                end: item["end"].as_f64().context("segment without end")?,
                text: item["text"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect::<Result<_>>()?;
    let language = json["language"].as_str().map(str::to_string);
    Ok((segments, language))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn on_path(names: &'static [&'static str]) -> impl Fn(&str) -> Option<PathBuf> {
        move |name| {
            names
                .contains(&name)
                .then(|| PathBuf::from(format!("/bin/{name}")))
        }
    }

    #[test]
    fn whisper_cpp_needs_a_model_file_and_falls_back_to_openai() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("ggml-base.en.bin");
        fs::write(&model, b"model").unwrap();
        let mut config = Config {
            model: Some(model.to_string_lossy().into_owned()),
            ..Config::default()
        };
        let both = on_path(&["whisper-cli", "whisper"]);
        let chosen = detect(&config, &both).unwrap();
        assert_eq!(chosen.engine, Engine::WhisperCpp);
        assert_eq!(chosen.provider(), "whisper.cpp:ggml-base.en");

        config.model = None;
        let chosen = detect(&config, &both).unwrap();
        assert_eq!(chosen.engine, Engine::OpenAi);
        assert_eq!(chosen.provider(), "whisper:base");

        let error = detect(&config, on_path(&["whisper-cli"])).unwrap_err();
        assert!(
            error.to_string().contains("ANYTOPDF_WHISPER_MODEL"),
            "{error}"
        );
        let error = detect(&config, on_path(&[])).unwrap_err();
        assert!(error.to_string().contains("no Whisper engine"), "{error}");
    }

    #[test]
    fn recorded_setup_model_enables_whisper_cpp_without_env() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("ggml-base.bin");
        fs::write(&model, b"model").unwrap();
        let config = Config {
            recorded_model: Some(model.clone()),
            ..Config::default()
        };
        let chosen = detect(&config, on_path(&["whisper-cli"])).unwrap();
        assert_eq!(chosen.engine, Engine::WhisperCpp);
        assert_eq!(chosen.provider(), "whisper.cpp:ggml-base");

        // An explicit model file still wins over the recorded one.
        let explicit = dir.path().join("ggml-small.bin");
        fs::write(&explicit, b"model").unwrap();
        let config = Config {
            model: Some(explicit.to_string_lossy().into_owned()),
            ..config
        };
        let chosen = detect(&config, on_path(&["whisper-cli"])).unwrap();
        assert_eq!(chosen.provider(), "whisper.cpp:ggml-small");

        // A model but no engine names the engine to install, not the setup step.
        let error = detect(&config, on_path(&[])).unwrap_err().to_string();
        assert!(error.contains("no Whisper engine"), "{error}");
        assert!(!error.contains("anytopdf setup whisper"), "{error}");
    }

    #[test]
    fn setup_steps_name_one_command_and_skip_the_model_once_installed() {
        let fresh = setup_steps(false);
        let modelled = setup_steps(true);
        assert!(fresh.starts_with("run `"), "{fresh}");
        assert!(fresh.contains("anytopdf setup whisper"), "{fresh}");
        assert!(!modelled.contains("anytopdf setup whisper"), "{modelled}");
        for steps in [&fresh, &modelled] {
            assert!(
                steps.contains("pipx install whisper-ctranslate2"),
                "{steps}"
            );
        }
        if cfg!(target_os = "macos") {
            assert!(fresh.contains("`brew install whisper-cpp && anytopdf setup whisper`"));
        }
    }

    #[test]
    fn readiness_explains_each_missing_piece() {
        let dir = tempfile::tempdir().unwrap();
        let model = dir.path().join("ggml-base.bin");
        fs::write(&model, b"model").unwrap();
        let bare = Config::default();
        let ready = readiness(&bare, on_path(&[]));
        assert!(!ready.ready);
        assert!(ready.detail.contains("anytopdf setup whisper"), "{ready:?}");

        let ready = readiness(&bare, on_path(&["whisper-cli", "ffmpeg"]));
        assert!(!ready.ready);
        assert!(ready.detail.contains("no ggml model"), "{ready:?}");
        assert!(ready.detail.contains("anytopdf setup whisper"), "{ready:?}");

        let configured = Config {
            recorded_model: Some(model),
            ..Config::default()
        };
        let ready = readiness(&configured, on_path(&["whisper-cli"]));
        assert!(!ready.ready);
        assert!(ready.detail.contains("ffmpeg"), "{ready:?}");

        let ready = readiness(&configured, on_path(&["whisper-cli", "ffmpeg"]));
        assert!(ready.ready, "{ready:?}");
        assert_eq!(ready.detail, "whisper.cpp:ggml-base via /bin/whisper-cli");
    }

    #[test]
    fn explicit_engine_and_binary_are_honoured() {
        let config = Config {
            engine: Some("openai-whisper".into()),
            model: Some("small".into()),
            ..Config::default()
        };
        let chosen = detect(&config, on_path(&["whisper-ctranslate2", "whisper"])).unwrap();
        assert_eq!(chosen.provider(), "whisper-ctranslate2:small");

        let config = Config {
            executable: Some(PathBuf::from("/opt/whisper.cpp/build/bin/whisper-cli")),
            ..Config::default()
        };
        let error = detect(&config, on_path(&[])).unwrap_err();
        assert!(error.to_string().contains("ggml model"), "{error}");

        let config = Config {
            engine: Some("nope".into()),
            ..Config::default()
        };
        assert!(detect(&config, on_path(&["whisper"])).is_err());
    }

    #[test]
    fn parses_whisper_cpp_millisecond_offsets() {
        let json = json!({
            "result": {"language": "en"},
            "transcription": [
                {"offsets": {"from": 0, "to": 1500}, "text": " Hello"},
                {"offsets": {"from": 1500, "to": 3250}, "text": " world"}
            ]
        });
        let (segments, language) = parse_whisper_cpp(&json).unwrap();
        assert_eq!(language.as_deref(), Some("en"));
        assert_eq!(segments[1].start, 1.5);
        assert_eq!(segments[1].end, 3.25);
        assert_eq!(segments[1].text, " world");
        assert!(parse_whisper_cpp(&json!({"text": "x"})).is_err());
    }

    #[test]
    fn parses_openai_second_offsets() {
        let json = json!({
            "language": "de",
            "segments": [{"start": 0.0, "end": 2.4, "text": " Guten Tag"}]
        });
        let (segments, language) = parse_openai(&json).unwrap();
        assert_eq!(language.as_deref(), Some("de"));
        assert_eq!(segments[0].end, 2.4);
        assert!(parse_openai(&json!({"segments": [{"text": "no times"}]})).is_err());
    }
}
