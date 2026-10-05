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
}

impl Config {
    pub fn from_env() -> Self {
        let var = |name: &str| env::var(name).ok().filter(|v| !v.trim().is_empty());
        Self {
            engine: var("ANYTOPDF_WHISPER_ENGINE"),
            executable: var("ANYTOPDF_WHISPER_BIN").map(PathBuf::from),
            model: var("ANYTOPDF_WHISPER_MODEL"),
            language: var("ANYTOPDF_WHISPER_LANGUAGE").filter(|l| l != "auto"),
        }
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
    let cpp_model = config.model.as_deref().filter(|m| Path::new(m).is_file());
    let backend = |engine, executable, model: &str| Backend {
        engine,
        executable,
        model: model.to_string(),
        language: config.language.clone(),
    };

    match engine {
        Some(Engine::WhisperCpp) => {
            let bin = find(&CPP_NAMES).context("whisper.cpp (whisper-cli) not found on PATH")?;
            let model = cpp_model
                .context("whisper.cpp needs ANYTOPDF_WHISPER_MODEL set to a ggml model file")?;
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
                    "whisper.cpp found but ANYTOPDF_WHISPER_MODEL does not name a ggml model file"
                );
            }
            bail!("no Whisper engine found; install whisper.cpp (whisper-cli) or openai-whisper")
        }
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
