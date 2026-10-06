//! `anytopdf-plugin-objects`: an anytopdf runtime plugin (protocol v1) that
//! detects objects in images and video keyframes with a YOLO ONNX model.
//!
//! It registers as a graph enricher so the model loads once per job, runs on
//! every visual unit of an image or video source, and adds one `object`
//! annotation per detection (label, confidence, normalized box, the frame's
//! time) plus a per-frame count such as "objects: 3 person, 1 dog". Inference
//! uses the pure-Rust runtime in `anytopdf-onnx`, so no Python or native
//! library is needed.

mod detector;
mod graph;
mod yolo;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{env, fs, path::Path, process::ExitCode};

const PROTOCOL: u64 = 1;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--anytopdf-manifest" => {
            println!("{}", manifest(readiness(|name| env::var(name).ok())));
            ExitCode::SUCCESS
        }
        [flag] if flag == "--version" => {
            println!("anytopdf-plugin-objects {}", env!("CARGO_PKG_VERSION"));
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
                    eprintln!("anytopdf-plugin-objects: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: anytopdf-plugin-objects --anytopdf-manifest\n       \
                 anytopdf-plugin-objects --anytopdf-request REQUEST --anytopdf-response RESPONSE"
            );
            ExitCode::from(2)
        }
    }
}

/// A copy bundled with anytopdf stays idle until a model is configured, so
/// conversions without one neither warn nor fail `--strict`. `None` is ready.
fn readiness(get: impl Fn(&str) -> Option<String>) -> Option<String> {
    match get("ANYTOPDF_OBJECTS_MODEL") {
        Some(model) if !model.trim().is_empty() => None,
        _ => Some("set ANYTOPDF_OBJECTS_MODEL to a YOLO .onnx model".into()),
    }
}

fn manifest(not_ready: Option<String>) -> Value {
    json!({
        "protocol": PROTOCOL,
        "name": "objects",
        "version": env!("CARGO_PKG_VERSION"),
        "ready": not_ready.is_none(),
        "detail": not_ready,
        "capabilities": [{
            "kind": "graph-enricher",
            "extensions": [],
            "mime_types": ["image/*", "video/*"],
            "priority": 50
        }]
    })
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
    let mut graph = request["graph"].clone();
    if !graph.is_object() {
        bail!("graph-enrich request has no graph");
    }

    let mut response = json!({"protocol": PROTOCOL, "ok": true, "warnings": []});
    let targets = graph::targets(&graph);
    if targets.is_empty() {
        return Ok(response);
    }
    let mut warnings = Vec::new();
    let detector = match detector::Config::from_env().and_then(|config| {
        if let Some(warning) = anytopdf_onnx::device_warning(config.device.as_deref()) {
            warnings.push(format!("objects: {warning}"));
        }
        detector::Detector::load(config)
    }) {
        Ok(detector) => detector,
        Err(e) => {
            warnings.push(format!(
                "objects: {e:#}; {} image(s) not analysed",
                targets.len()
            ));
            response["warnings"] = json!(warnings);
            return Ok(response);
        }
    };

    let mut changed = false;
    for target in &targets {
        let result = image::ImageReader::open(&target.path)
            .context("open image")
            .and_then(|r| r.with_guessed_format().context("read image"))
            .and_then(|r| r.decode().context("decode image"))
            .and_then(|image| detector.detect(&image));
        match result {
            Ok(objects) => {
                changed |= graph::apply(&mut graph, target, &objects, &detector.model_name);
            }
            Err(e) => warnings.push(format!(
                "objects: {}: {e:#}",
                target
                    .path
                    .file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            )),
        }
    }
    response["warnings"] = json!(warnings);
    if changed {
        response["graph"] = graph;
    }
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
mod manifest_tests {
    use super::*;

    #[test]
    fn bundled_copy_is_idle_until_a_model_is_set() {
        let idle = manifest(readiness(|_| None));
        assert_eq!(idle["ready"], false);
        assert!(
            idle["detail"]
                .as_str()
                .unwrap()
                .contains("ANYTOPDF_OBJECTS_MODEL")
        );
        assert_eq!(idle["capabilities"].as_array().unwrap().len(), 1);

        let ready = manifest(readiness(|name| {
            (name == "ANYTOPDF_OBJECTS_MODEL").then(|| "/m/yolo11n.onnx".into())
        }));
        assert_eq!(ready["ready"], true);
        assert!(ready["detail"].is_null());
    }
}
