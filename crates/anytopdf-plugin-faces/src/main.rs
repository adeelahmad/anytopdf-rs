//! `anytopdf-plugin-faces`: an anytopdf runtime plugin (protocol v1) that
//! detects faces in images and video keyframes.
//!
//! It registers as a unit enricher for visual units and appends one `face`
//! annotation per detected face (normalized box, confidence, five landmarks)
//! plus one per-unit count. It records only these neutral facts: it never
//! estimates age, gender, emotion or any other trait. The YuNet detector is
//! compiled in, so it needs no download, Python or native library.

mod align;
mod annotate;
mod detector;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

const PROTOCOL: u64 = 1;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--anytopdf-manifest" => {
            println!("{}", manifest());
            ExitCode::SUCCESS
        }
        [flag] if flag == "--version" => {
            println!("anytopdf-plugin-faces {}", env!("CARGO_PKG_VERSION"));
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
                    eprintln!("anytopdf-plugin-faces: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: anytopdf-plugin-faces --anytopdf-manifest\n       \
                 anytopdf-plugin-faces --anytopdf-request REQUEST --anytopdf-response RESPONSE"
            );
            ExitCode::from(2)
        }
    }
}

fn manifest() -> Value {
    json!({
        "protocol": PROTOCOL,
        "name": "faces",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": [{
            "kind": "unit-enricher",
            "extensions": [],
            "mime_types": ["image/*", "video/*"],
            "priority": 50
        }]
    })
}

/// Settings read from `ANYTOPDF_FACES*` environment variables.
#[derive(Debug, Clone, PartialEq)]
struct Config {
    enabled: bool,
    threshold: f32,
    min_size: f32,
    input_size: u32,
    model: Option<PathBuf>,
    crops: bool,
}

impl Config {
    fn from_env() -> Result<Self> {
        Self::from_lookup(|name| env::var(name).ok().filter(|v| !v.trim().is_empty()))
    }

    fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let switch = |name: &str| -> Result<bool> {
            match get(name).map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                None | Some("on" | "1" | "true" | "yes") => Ok(true),
                Some("off" | "0" | "false" | "no") => Ok(false),
                Some(other) => bail!("{name} must be on or off, not {other:?}"),
            }
        };
        let number = |name: &str, default: f32| -> Result<f32> {
            match get(name) {
                None => Ok(default),
                Some(raw) => raw
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .with_context(|| format!("{name} must be a nonnegative number, not {raw:?}")),
            }
        };
        let threshold = number("ANYTOPDF_FACES_THRESHOLD", 0.8)?;
        if threshold > 1.0 {
            bail!("ANYTOPDF_FACES_THRESHOLD must be between 0 and 1, not {threshold}");
        }
        let input_size = number("ANYTOPDF_FACES_INPUT_SIZE", 640.0)?;
        if !(32.0..=4096.0).contains(&input_size) {
            bail!("ANYTOPDF_FACES_INPUT_SIZE must be between 32 and 4096, not {input_size}");
        }
        Ok(Self {
            enabled: switch("ANYTOPDF_FACES")?,
            threshold,
            min_size: number("ANYTOPDF_FACES_MIN_SIZE", 20.0)?,
            input_size: input_size as u32,
            model: get("ANYTOPDF_FACES_MODEL").map(PathBuf::from),
            crops: switch("ANYTOPDF_FACES_CROPS")?,
        })
    }

    fn detector(&self) -> (detector::Detector, String) {
        let (mut detector, name) = match &self.model {
            Some(path) => (
                detector::Detector::from_path(path),
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "custom".into()),
            ),
            None => (
                detector::Detector::embedded(),
                detector::EMBEDDED_MODEL_NAME.into(),
            ),
        };
        detector.score_threshold = self.threshold;
        detector.min_size = self.min_size;
        detector.input_size = self.input_size;
        (detector, name)
    }
}

fn handle(request_path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(request_path).context("read request")?;
    let request: Value = serde_json::from_str(&raw).context("parse request")?;
    if request["protocol"].as_u64() != Some(PROTOCOL) {
        bail!("unsupported protocol {}", request["protocol"]);
    }
    if request["operation"] != "unit-enrich" {
        bail!("unsupported operation {}", request["operation"]);
    }
    let workspace = PathBuf::from(
        request["workspace"]
            .as_str()
            .context("request has no workspace")?,
    );
    let unit = &request["unit"];
    let mut response = json!({"protocol": PROTOCOL, "ok": true, "warnings": [], "annotations": []});
    let config = Config::from_env()?;
    if !config.enabled || unit["kind"] != "visual" || annotate::already_detected(unit) {
        return Ok(response);
    }
    let Some(visual) = unit["visual_path"].as_str() else {
        return Ok(response);
    };
    let image = detector::load_rgb(Path::new(visual))?;
    let (detector, model_name) = config.detector();
    let faces = detector.detect(&image)?;
    let crops = if config.crops && !faces.is_empty() {
        match annotate::write_crops(&workspace, unit, &image, &faces) {
            Ok(crops) => crops,
            Err(e) => {
                response["warnings"] = json!([format!("faces: crops not saved: {e:#}")]);
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    response["annotations"] = json!(annotate::annotations(
        unit,
        image.dimensions(),
        &faces,
        &crops,
        &model_name
    ));
    Ok(response)
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
    use std::collections::HashMap;

    fn config(pairs: &[(&str, &str)]) -> Result<Config> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn config_defaults_to_enabled_with_crops() {
        let c = config(&[]).unwrap();
        assert!(c.enabled && c.crops);
        assert_eq!((c.threshold, c.min_size, c.input_size), (0.8, 20.0, 640));
        assert_eq!(c.model, None);
    }

    #[test]
    fn config_reads_switches_and_numbers() {
        let c = config(&[
            ("ANYTOPDF_FACES", "off"),
            ("ANYTOPDF_FACES_CROPS", "0"),
            ("ANYTOPDF_FACES_THRESHOLD", "0.6"),
            ("ANYTOPDF_FACES_MIN_SIZE", "40"),
            ("ANYTOPDF_FACES_INPUT_SIZE", "1280"),
            ("ANYTOPDF_FACES_MODEL", "/models/yunet.onnx"),
        ])
        .unwrap();
        assert!(!c.enabled && !c.crops);
        assert_eq!((c.threshold, c.min_size, c.input_size), (0.6, 40.0, 1280));
        assert_eq!(c.detector().1, "yunet");
    }

    #[test]
    fn config_rejects_bad_values_by_name() {
        for (name, value) in [
            ("ANYTOPDF_FACES", "maybe"),
            ("ANYTOPDF_FACES_THRESHOLD", "1.5"),
            ("ANYTOPDF_FACES_MIN_SIZE", "-1"),
            ("ANYTOPDF_FACES_INPUT_SIZE", "8"),
        ] {
            let error = config(&[(name, value)]).unwrap_err().to_string();
            assert!(error.contains(name), "{error}");
        }
    }
}
