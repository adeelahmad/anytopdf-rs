//! `anytopdf-plugin-face-id`: an anytopdf runtime plugin (protocol v1) that
//! computes a face embedding for every detected face.
//!
//! It registers as a graph enricher in the `after-units` phase, so it runs
//! once per job after the face detector has added `face` annotations with five-point `landmarks`
//! (or an aligned `crop`). It aligns each face onto the ArcFace template,
//! embeds it with a user-supplied ONNX model, tags the annotation with a
//! `face.ref` and writes the vectors to `face-id/` in the job workspace.
//! Embeddings never enter the graph; the host matches them against the local
//! face index when `--recognize-faces` is on. Nothing here estimates age,
//! gender, emotion or any other trait.

mod embed;

use anyhow::{Context, Result, bail};
use anytopdf_faces::{align, paths, workspace};
use image::RgbImage;
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

const PROTOCOL: u64 = 1;
const MODEL_ENV: &str = "ANYTOPDF_FACE_EMBED_MODEL";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--anytopdf-manifest" => {
            println!("{}", manifest(readiness()));
            ExitCode::SUCCESS
        }
        [flag] if flag == "--version" => {
            println!("anytopdf-plugin-face-id {}", env!("CARGO_PKG_VERSION"));
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
                    eprintln!("anytopdf-plugin-face-id: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: anytopdf-plugin-face-id --anytopdf-manifest\n       \
                 anytopdf-plugin-face-id --anytopdf-request REQUEST --anytopdf-response RESPONSE"
            );
            ExitCode::from(2)
        }
    }
}

/// A copy bundled with anytopdf stays idle until an embedding model is set or
/// installed, so face detection without one neither warns nor fails `--strict`.
/// `None` is ready.
fn readiness() -> Option<String> {
    let explicit = env::var_os(MODEL_ENV).is_some_and(|v| !v.is_empty());
    if explicit || model_path().is_some_and(|path| path.is_file()) {
        None
    } else {
        Some(format!(
            "set {MODEL_ENV} to an ArcFace-style ONNX face embedding model"
        ))
    }
}

fn manifest(not_ready: Option<String>) -> Value {
    json!({
        "protocol": PROTOCOL,
        "name": "face-id",
        "version": env!("CARGO_PKG_VERSION"),
        "ready": not_ready.is_none(),
        "detail": not_ready,
        "capabilities": [{
            "kind": "graph-enricher",
            "extensions": [],
            "mime_types": [],
            "priority": 0,
            "phase": "after-units"
        }]
    })
}

/// `ANYTOPDF_FACE_EMBED_MODEL`, else `models/face-embedding.onnx` in the
/// anytopdf data directory.
fn model_path() -> Option<PathBuf> {
    env::var_os(MODEL_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| paths::data_dir().map(|d| d.join("models").join("face-embedding.onnx")))
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
    let mut response = json!({"protocol": PROTOCOL, "ok": true, "warnings": []});
    let pending = count_pending(&graph);
    if pending == 0 {
        return Ok(response);
    }
    let embedder = match model_path() {
        Some(path) if path.is_file() => embed::Embedder::load(
            &path,
            embed::input_scale_override(env::var(embed::INPUT_ENV).ok().as_deref())?,
        )
        .with_context(|| format!("load face embedding model {}", path.display()))?,
        _ => {
            response["warnings"] = json!([format!(
                "face-id: no face embedding model; set {MODEL_ENV} to an ArcFace-style ONNX file. \
                 {pending} face(s) not embedded"
            )]);
            return Ok(response);
        }
    };
    let (faces, warnings) = embed_graph(&mut graph, &workspace, &embedder);
    response["warnings"] = json!(warnings);
    if !faces.is_empty() {
        workspace::write(
            &workspace,
            &workspace::EmbeddingFile {
                model: embedder.id.clone(),
                faces,
            },
        )?;
        response["graph"] = graph;
    }
    Ok(response)
}

fn pending_faces(unit: &Value) -> Vec<usize> {
    let Some(annotations) = unit["annotations"].as_array() else {
        return Vec::new();
    };
    annotations
        .iter()
        .enumerate()
        .filter(|(_, a)| {
            let attrs = &a["attributes"];
            a["kind"] == "face"
                && attrs.get(workspace::REF_ATTR).is_none()
                && (attrs.get(workspace::LANDMARKS_ATTR).is_some()
                    || attrs.get(workspace::CROP_ATTR).is_some())
        })
        .map(|(i, _)| i)
        .collect()
}

fn count_pending(graph: &Value) -> usize {
    graph["units"].as_array().map_or(0, |units| {
        units.iter().map(|u| pending_faces(u).len()).sum()
    })
}

/// Embeds every pending face, tagging each with a fresh reference. Faces that
/// fail are left untagged and reported once per unit.
fn embed_graph(
    graph: &mut Value,
    workspace: &Path,
    embedder: &embed::Embedder,
) -> (Vec<workspace::FaceEmbedding>, Vec<String>) {
    let mut faces = Vec::new();
    let mut warnings = Vec::new();
    let Some(units) = graph["units"].as_array_mut() else {
        return (faces, warnings);
    };
    for unit in units {
        let pending = pending_faces(unit);
        if pending.is_empty() {
            continue;
        }
        let needs_image = pending.iter().any(|&i| {
            unit["annotations"][i]["attributes"]
                .get(workspace::CROP_ATTR)
                .is_none()
        });
        let image = needs_image.then(|| {
            unit_visual(unit)
                .and_then(|path| {
                    Ok(image::open(&path)
                        .with_context(|| format!("decode {}", path.display()))?
                        .to_rgb8())
                })
                .map_err(|e| format!("{e:#}"))
        });
        let mut failed = Vec::new();
        for i in pending {
            let annotation = &mut unit["annotations"][i];
            let crop = crop_for(annotation, workspace, embedder.size, image.as_ref());
            match crop.and_then(|c| embedder.embed(&c)) {
                Ok(embedding) => {
                    let face_ref = uuid::Uuid::new_v4().to_string();
                    annotation["attributes"][workspace::REF_ATTR] = json!(face_ref);
                    faces.push(workspace::FaceEmbedding {
                        face_ref,
                        embedding,
                    });
                }
                Err(e) => failed.push(format!("{e:#}")),
            }
        }
        if !failed.is_empty() {
            let count = failed.len();
            failed.sort();
            failed.dedup();
            warnings.push(format!(
                "face-id: {count} face(s) not embedded: {}",
                failed.join("; ")
            ));
        }
    }
    (faces, warnings)
}

fn unit_visual(unit: &Value) -> Result<PathBuf> {
    unit["visual_path"]
        .as_str()
        .map(PathBuf::from)
        .context("face annotation on a unit without an image")
}

/// The aligned crop: the detector's own `crop` when it lies inside the
/// workspace, else the unit image aligned on the face's landmarks.
fn crop_for(
    annotation: &Value,
    workspace: &Path,
    size: u32,
    image: Option<&Result<RgbImage, String>>,
) -> Result<RgbImage> {
    let attrs = &annotation["attributes"];
    if let Some(crop) = attrs[workspace::CROP_ATTR].as_str() {
        // Relative crops are relative to the workspace.
        let root = workspace.canonicalize().context("resolve workspace")?;
        let path = root
            .join(crop)
            .canonicalize()
            .context("resolve face crop")?;
        if !path.starts_with(&root) {
            bail!("face crop lies outside the job workspace");
        }
        let crop = image::open(&path).context("decode face crop")?.to_rgb8();
        return Ok(if crop.dimensions() == (size, size) {
            crop
        } else {
            image::imageops::resize(&crop, size, size, image::imageops::FilterType::Triangle)
        });
    }
    let landmarks = align::parse_landmarks(
        attrs[workspace::LANDMARKS_ATTR]
            .as_str()
            .context("landmarks must be a string")?,
    )?;
    let image = match image {
        Some(Ok(image)) => image,
        Some(Err(e)) => bail!("{e}"),
        None => bail!("face annotation on a unit without an image"),
    };
    align::align(image, &landmarks, size)
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
    fn manifest_reports_idle_with_what_is_missing() {
        let idle = manifest(Some("needs a model".into()));
        assert_eq!(idle["ready"], false);
        assert_eq!(idle["detail"], "needs a model");
        assert_eq!(idle["capabilities"].as_array().unwrap().len(), 1);
        let ready = manifest(None);
        assert_eq!(ready["ready"], true);
        assert!(ready["detail"].is_null());
    }
}
