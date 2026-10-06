//! `anytopdf-plugin-clip`: an anytopdf runtime plugin (protocol v1) that embeds
//! every visual unit with CLIP for search by meaning, and tags it with
//! zero-shot scene labels ("screenshot", "outdoors", "beach") that land in the
//! PDF's searchable layer.
//!
//! It registers as a graph enricher so the model loads once per conversion.
//! Each embedding is stored in the unit's metadata (`clip.embedding`,
//! base64 little-endian f32, unit length) for the search index; renderers never
//! write unit metadata into the PDF. `--encode-text` and `--encode-image` embed
//! a search query with the same model.

mod encoder;
mod fetch;
mod onnx;
mod tags;
mod tokenizer;

use anyhow::{Context, Result, bail};
use base64::Engine;
use encoder::{Clip, Encoder};
use serde_json::{Map, Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

const PROTOCOL: u64 = 1;
const PROVIDER: &str = "clip";
const DEFAULT_TAG_THRESHOLD: f32 = 0.5;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--anytopdf-manifest"] => {
            println!("{}", manifest());
            Ok(())
        }
        ["--version"] => {
            println!("anytopdf-plugin-clip {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        [
            "--anytopdf-request",
            request,
            "--anytopdf-response",
            response,
        ] => {
            let body = handle(Path::new(request)).unwrap_or_else(|e| {
                json!({"protocol": PROTOCOL, "ok": false, "warnings": [], "error": format!("{e:#}")})
            });
            write_atomic(Path::new(response), &body)
        }
        ["--encode-text", text] => Config::from_env()
            .open()
            .and_then(|clip| print_vector(&clip, clip.encode_text(text))),
        ["--encode-image", path] => Config::from_env()
            .open()
            .and_then(|clip| print_vector(&clip, clip.encode_image(Path::new(path)))),
        ["--fetch-model", rest @ ..] if rest.len() <= 1 => {
            let dir = rest
                .first()
                .map(PathBuf::from)
                .or_else(|| Config::from_env().model_dir)
                .context("no model directory: pass one or set ANYTOPDF_CLIP_MODEL_DIR")
                .and_then(|dir| {
                    let mirror = env::var("ANYTOPDF_CLIP_MODEL_MIRROR").ok();
                    fetch::fetch(&dir, fetch::DEFAULT_FILES, mirror.as_deref())?;
                    Ok(dir)
                });
            dir.map(|dir| {
                eprintln!(
                    "CLIP model ready in {}. Set ANYTOPDF_CLIP_MODEL_DIR to it if that is not the default.",
                    dir.display()
                )
            })
        }
        _ => {
            eprintln!(
                "usage: anytopdf-plugin-clip --anytopdf-manifest\n       \
                 anytopdf-plugin-clip --anytopdf-request REQUEST --anytopdf-response RESPONSE\n       \
                 anytopdf-plugin-clip --encode-text TEXT | --encode-image IMAGE\n       \
                 anytopdf-plugin-clip --fetch-model [DIR]"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("anytopdf-plugin-clip: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn manifest() -> Value {
    json!({
        "protocol": PROTOCOL,
        "name": PROVIDER,
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": [{
            "kind": "graph-enricher",
            "extensions": [],
            "mime_types": [],
            "priority": 40
        }]
    })
}

/// Settings from the environment.
struct Config {
    model_dir: Option<PathBuf>,
    tags: Option<String>,
    threshold: f32,
    device: Option<String>,
}

impl Config {
    fn from_env() -> Self {
        let var = |name: &str| env::var(name).ok().filter(|v| !v.trim().is_empty());
        Self {
            model_dir: var("ANYTOPDF_CLIP_MODEL_DIR")
                .map(PathBuf::from)
                .or_else(default_model_dir),
            tags: var("ANYTOPDF_CLIP_TAGS"),
            threshold: var("ANYTOPDF_CLIP_TAG_THRESHOLD")
                .and_then(|v| v.parse::<f32>().ok())
                .filter(|v| (0.0..=1.0).contains(v))
                .unwrap_or(DEFAULT_TAG_THRESHOLD),
            device: var("ANYTOPDF_CLIP_DEVICE"),
        }
    }

    fn open(&self) -> Result<Clip> {
        let dir = self.model_dir.as_ref().context(
            "no CLIP model: run `anytopdf-plugin-clip --fetch-model` or set ANYTOPDF_CLIP_MODEL_DIR",
        )?;
        Clip::open(dir).context(
            "no usable CLIP model: run `anytopdf-plugin-clip --fetch-model` or set ANYTOPDF_CLIP_MODEL_DIR",
        )
    }
}

/// `<user data dir>/anytopdf/models/clip`, where `--fetch-model` puts the model.
fn default_model_dir() -> Option<PathBuf> {
    let home = || env::var_os("HOME").map(PathBuf::from);
    let base = if cfg!(windows) {
        env::var_os("LOCALAPPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Application Support"))
    } else {
        env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home().map(|h| h.join(".local/share")))
    }?;
    Some(base.join("anytopdf").join("models").join("clip"))
}

fn print_vector(clip: &Clip, vector: Result<Vec<f32>>) -> Result<()> {
    let vector = vector?;
    println!(
        "{}",
        json!({"model": clip.model_id(), "dim": vector.len(), "embedding": vector})
    );
    Ok(())
}

pub fn encode_base64(vector: &[f32]) -> String {
    let bytes: Vec<u8> = vector.iter().flat_map(|v| v.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
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
    let config = Config::from_env();
    let mut response = json!({"protocol": PROTOCOL, "ok": true, "warnings": []});
    let pending = visual_units(&graph);
    if pending.is_empty() {
        return Ok(response);
    }
    let mut warnings = Vec::new();
    if let Some(device) = config
        .device
        .as_deref()
        .filter(|d| !d.eq_ignore_ascii_case("cpu"))
    {
        warnings.push(format!(
            "clip: device {device} is not available in this build; running on the CPU"
        ));
    }
    let clip = match config.open() {
        Ok(clip) => clip,
        Err(e) => {
            warnings.push(format!(
                "clip: {e:#}; {} image(s) not embedded",
                pending.len()
            ));
            response["warnings"] = json!(warnings);
            return Ok(response);
        }
    };
    let changed = enrich(&mut graph, &pending, &clip, &config, &mut warnings);
    response["warnings"] = json!(warnings);
    if changed {
        response["graph"] = graph;
    }
    Ok(response)
}

/// Indexes of visual units with an image that this model has not embedded yet.
fn visual_units(graph: &Value) -> Vec<usize> {
    let model_of = |unit: &Value| unit["metadata"]["clip.model"].as_str().map(str::to_owned);
    graph["units"]
        .as_array()
        .map(|units| {
            units
                .iter()
                .enumerate()
                .filter(|(_, u)| u["kind"] == "visual" && u["visual_path"].is_string())
                .filter(|(_, u)| model_of(u).is_none())
                .map(|(i, _)| i)
                .collect()
        })
        .unwrap_or_default()
}

fn enrich(
    graph: &mut Value,
    pending: &[usize],
    clip: &dyn Encoder,
    config: &Config,
    warnings: &mut Vec<String>,
) -> bool {
    let groups = tags::parse_groups(config.tags.as_deref());
    let prompts: Vec<Vec<Vec<f32>>> = match groups
        .iter()
        .map(|g| g.tags.iter().map(|t| clip.encode_text(&t.prompt)).collect())
        .collect::<Result<_>>()
    {
        Ok(prompts) => prompts,
        Err(e) => {
            warnings.push(format!("clip: scene tags disabled: {e:#}"));
            Vec::new()
        }
    };
    let mut changed = false;
    for &index in pending {
        let unit = &mut graph["units"][index];
        let Some(path) = unit["visual_path"].as_str().map(PathBuf::from) else {
            continue;
        };
        let vector = match clip.encode_image(&path) {
            Ok(vector) => vector,
            Err(e) => {
                warnings.push(format!("clip: {}: {e:#}", path.display()));
                continue;
            }
        };
        if !unit["metadata"].is_object() {
            unit["metadata"] = Value::Object(Map::new());
        }
        let metadata = &mut unit["metadata"];
        metadata["clip.embedding"] = json!(encode_base64(&vector));
        metadata["clip.model"] = json!(clip.model_id());
        metadata["clip.dim"] = json!(vector.len().to_string());
        if !prompts.is_empty() {
            if !unit["annotations"].is_array() {
                unit["annotations"] = json!([]);
            }
            for chosen in tags::choose(&groups, &prompts, &vector, config.threshold) {
                if let Some(annotations) = unit["annotations"].as_array_mut() {
                    annotations.push(json!({
                        "kind": "scene",
                        "text": chosen.label,
                        "provider": PROVIDER,
                        "confidence": chosen.probability,
                        "region": null,
                        "time_range": null,
                        "attributes": {
                            "entity": "scene-tag",
                            "group": chosen.group,
                            "prompt": chosen.prompt,
                            "model": clip.model_id(),
                        }
                    }));
                }
            }
        }
        changed = true;
    }
    changed
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
