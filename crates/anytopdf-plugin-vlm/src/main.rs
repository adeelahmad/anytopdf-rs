//! `anytopdf-plugin-vlm`: an anytopdf runtime plugin (protocol v1) that
//! describes images and video keyframes with a local vision-language model.
//!
//! As a unit enricher it asks each keyframe a list of prompts (by default a
//! caption, "what do you see" and the activities shown) and can run
//! open-vocabulary detection. As an after-units graph enricher it writes a
//! summary page with a category and topics for each video, audio or
//! transcript source, and per-scene summaries for videos. It talks
//! to an OpenAI-compatible endpoint (Ollama, llama.cpp, LM Studio) or the
//! Moondream API, and stays inert until `ANYTOPDF_VLM_URL` is set.

mod client;
mod config;
mod frames;
mod summary;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{env, fs, path::Path, process::ExitCode, time::Instant};

const PROTOCOL: u64 = 1;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--anytopdf-manifest" => {
            println!("{}", manifest(config::Config::from_env()));
            ExitCode::SUCCESS
        }
        [flag] if flag == "--version" => {
            println!("anytopdf-plugin-vlm {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [req_flag, request, resp_flag, response]
            if req_flag == "--anytopdf-request" && resp_flag == "--anytopdf-response" =>
        {
            let started = Instant::now();
            let body = handle(Path::new(request), started).unwrap_or_else(|e| {
                json!({"protocol": PROTOCOL, "ok": false, "warnings": [], "error": format!("{e:#}")})
            });
            match write_atomic(Path::new(response), &body) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("anytopdf-plugin-vlm: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: anytopdf-plugin-vlm --anytopdf-manifest\n       \
                 anytopdf-plugin-vlm --anytopdf-request REQUEST --anytopdf-response RESPONSE"
            );
            ExitCode::from(2)
        }
    }
}

/// Declares no capabilities until an endpoint is configured, so a bundled
/// copy on `ANYTOPDF_PLUGIN_PATH` costs nothing and warns about nothing. A
/// misconfiguration still registers the unit enricher so its error surfaces.
fn manifest(config: Result<Option<config::Config>>) -> Value {
    let mut capabilities = Vec::new();
    if !matches!(config, Ok(None)) {
        capabilities.push(json!({
            "kind": "unit-enricher",
            "extensions": [],
            "mime_types": ["image/*", "video/*"],
            "priority": 40
        }));
    }
    if let Ok(Some(config)) = config
        && config.text.is_some()
    {
        capabilities.push(json!({
            "kind": "graph-enricher",
            "extensions": [],
            "mime_types": ["video/*"],
            "priority": 40,
            "phase": "after-units"
        }));
    }
    json!({
        "protocol": PROTOCOL,
        "name": "vlm",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": capabilities
    })
}

fn handle(request_path: &Path, started: Instant) -> Result<Value> {
    let raw = fs::read_to_string(request_path).context("read request")?;
    let request: Value = serde_json::from_str(&raw).context("parse request")?;
    if request["protocol"].as_u64() != Some(PROTOCOL) {
        bail!("unsupported protocol {}", request["protocol"]);
    }
    let mut response = json!({"protocol": PROTOCOL, "ok": true, "warnings": []});
    let Some(config) = config::Config::from_env()? else {
        return Ok(response);
    };
    let deadline = started + config.budget;
    match request["operation"].as_str() {
        Some("unit-enrich") => {
            let unit = &request["unit"];
            if !unit.is_object() {
                bail!("unit-enrich request has no unit");
            }
            let client = client::Client::new(config.vision.clone(), deadline);
            let (annotations, warnings) = frames::describe_unit(&config, &client, unit);
            response["annotations"] = json!(annotations);
            response["warnings"] = json!(warnings);
        }
        Some("graph-enrich") => {
            let Some(text) = config.text.clone() else {
                return Ok(response);
            };
            let mut graph = request["graph"].clone();
            if !graph.is_object() {
                bail!("graph-enrich request has no graph");
            }
            let client = client::Client::new(text, deadline);
            let (changed, warnings) = summary::summarize(&config, &client, &mut graph);
            response["warnings"] = json!(warnings);
            if changed {
                response["graph"] = graph;
            }
        }
        _ => bail!("unsupported operation {}", request["operation"]),
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
