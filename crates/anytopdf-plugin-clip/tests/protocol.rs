//! End-to-end protocol tests against the tiny fixture model in
//! `tests/fixtures/tiny-clip` (see `make_tiny_clip.py`): red, green and blue
//! images embed along the x, y and z axes, as do the words "red", "green" and
//! "blue".

use base64::Engine;
use image::{Rgb, RgbImage};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tiny-clip")
}

fn plugin(model_dir: Option<&Path>) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf-plugin-clip"));
    for name in [
        "ANYTOPDF_CLIP_MODEL_DIR",
        "ANYTOPDF_CLIP_TAGS",
        "ANYTOPDF_CLIP_TAG_THRESHOLD",
        "ANYTOPDF_CLIP_DEVICE",
    ] {
        command.env_remove(name);
    }
    match model_dir {
        Some(dir) => command.env("ANYTOPDF_CLIP_MODEL_DIR", dir),
        // An empty home and data dir, so no real model is picked up.
        None => command
            .env("ANYTOPDF_CLIP_MODEL_DIR", "")
            .env("HOME", "/nonexistent")
            .env("XDG_DATA_HOME", "/nonexistent")
            .env("LOCALAPPDATA", "/nonexistent"),
    };
    command.env("ANYTOPDF_CLIP_TAGS", "red;green;blue");
    command
}

fn solid(dir: &Path, name: &str, rgb: [u8; 3]) -> PathBuf {
    let path = dir.join(name);
    RgbImage::from_pixel(32, 24, Rgb(rgb)).save(&path).unwrap();
    path
}

fn unit(n: u32, kind: &str, visual: Option<&Path>) -> Value {
    json!({"id": format!("aaaaaaaa-aaaa-4aaa-8aaa-{n:012}"),
           "source_id": "11111111-1111-4111-8111-111111111111", "kind": kind,
           "visual_path": visual, "visible_text": null, "time_range": null,
           "annotations": [], "metadata": {"keep": "me"}})
}

fn graph(units: Vec<Value>) -> Value {
    json!({
        "sources": [{"id": "11111111-1111-4111-8111-111111111111",
                     "path": "/data/input", "detected_type": "image/png", "metadata": {}}],
        "units": units,
        "metadata": {}
    })
}

fn exchange(mut command: Command, workspace: &Path, graph: Value) -> Value {
    let request = workspace.join("request.json");
    let response = workspace.join("response.json");
    fs::write(
        &request,
        serde_json::to_vec(&json!({
            "protocol": 1, "operation": "graph-enrich", "workspace": workspace,
            "source": null, "unit": null, "graph": graph, "output": null,
            "future_request_field": true
        }))
        .unwrap(),
    )
    .unwrap();
    let status = command
        .arg("--anytopdf-request")
        .arg(&request)
        .arg("--anytopdf-response")
        .arg(&response)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!workspace.join("response.json.tmp").exists());
    serde_json::from_slice(&fs::read(&response).unwrap()).unwrap()
}

fn decode(base64: &str) -> Vec<f32> {
    base64::engine::general_purpose::STANDARD
        .decode(base64)
        .unwrap()
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn tags(unit: &Value) -> Vec<String> {
    unit["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["kind"] == "scene")
        .map(|a| a["text"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn manifest_declares_a_protocol_v1_graph_enricher() {
    let output = plugin(None).arg("--anytopdf-manifest").output().unwrap();
    assert!(output.status.success());
    let manifest: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(manifest["protocol"], 1);
    assert_eq!(manifest["name"], "clip");
    assert_eq!(manifest["capabilities"][0]["kind"], "graph-enricher");
}

#[test]
fn embeds_visual_units_and_tags_them() {
    let dir = tempfile::tempdir().unwrap();
    let red = solid(dir.path(), "red.png", [255, 0, 0]);
    let blue = solid(dir.path(), "blue.png", [0, 0, 255]);
    let input = graph(vec![
        unit(1, "visual", Some(&red)),
        unit(2, "text", None),
        unit(3, "visual", Some(&blue)),
    ]);
    let response = exchange(plugin(Some(&fixture())), dir.path(), input);
    assert_eq!(response["ok"], true);
    assert_eq!(response["warnings"], json!([]));
    let units = response["graph"]["units"].as_array().unwrap();

    let red_unit = &units[0];
    let metadata = &red_unit["metadata"];
    assert_eq!(metadata["keep"], "me");
    assert!(
        metadata["clip.model"]
            .as_str()
            .unwrap()
            .starts_with("clip-")
    );
    assert_eq!(metadata["clip.dim"], "3");
    let vector = decode(metadata["clip.embedding"].as_str().unwrap());
    let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4, "embedding is unit length");
    assert!(
        vector[0] > vector[1] && vector[0] > vector[2],
        "red points along x"
    );

    assert_eq!(tags(red_unit), ["red"]);
    assert_eq!(tags(&units[2]), ["blue"]);
    let tag = &red_unit["annotations"][0];
    assert_eq!(tag["provider"], "clip");
    assert_eq!(tag["attributes"]["entity"], "scene-tag");
    assert!(tag["confidence"].as_f64().unwrap() > 0.9);

    // Text units are left exactly as they were.
    assert_eq!(units[1], unit(2, "text", None));
}

#[test]
fn units_already_embedded_with_this_model_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let red = solid(dir.path(), "red.png", [255, 0, 0]);
    let first = exchange(
        plugin(Some(&fixture())),
        dir.path(),
        graph(vec![unit(1, "visual", Some(&red))]),
    );
    let second = exchange(plugin(Some(&fixture())), dir.path(), first["graph"].clone());
    assert_eq!(second["ok"], true);
    assert!(second.get("graph").is_none(), "nothing left to change");
}

#[test]
fn a_missing_model_is_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let red = solid(dir.path(), "red.png", [255, 0, 0]);
    let response = exchange(
        plugin(None),
        dir.path(),
        graph(vec![unit(1, "visual", Some(&red))]),
    );
    assert_eq!(response["ok"], true);
    assert!(response.get("graph").is_none());
    let warning = response["warnings"][0].as_str().unwrap();
    assert!(warning.contains("--fetch-model"), "{warning}");
    assert!(warning.contains("1 image(s) not embedded"), "{warning}");
}

#[test]
fn an_unreadable_image_warns_and_the_rest_still_embed() {
    let dir = tempfile::tempdir().unwrap();
    let broken = dir.path().join("broken.png");
    fs::write(&broken, b"not a png").unwrap();
    let green = solid(dir.path(), "green.png", [0, 255, 0]);
    let response = exchange(
        plugin(Some(&fixture())),
        dir.path(),
        graph(vec![
            unit(1, "visual", Some(&broken)),
            unit(2, "visual", Some(&green)),
        ]),
    );
    let warnings = response["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].as_str().unwrap().contains("broken.png"));
    let units = response["graph"]["units"].as_array().unwrap();
    assert!(units[0]["metadata"].get("clip.embedding").is_none());
    assert_eq!(tags(&units[1]), ["green"]);
}

#[test]
fn encode_text_and_image_share_one_space() {
    let dir = tempfile::tempdir().unwrap();
    let blue = solid(dir.path(), "blue.png", [0, 0, 255]);
    let run = |args: &[&std::ffi::OsStr]| -> Value {
        let output = plugin(Some(&fixture())).args(args).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        serde_json::from_slice(&output.stdout).unwrap()
    };
    let text = run(&["--encode-text".as_ref(), "Blue".as_ref()]);
    let image = run(&["--encode-image".as_ref(), blue.as_os_str()]);
    assert_eq!(text["model"], image["model"]);
    assert_eq!(text["dim"], 3);
    let dot: f64 = (0..3)
        .map(|i| text["embedding"][i].as_f64().unwrap() * image["embedding"][i].as_f64().unwrap())
        .sum();
    assert!(dot > 0.5, "blue text matches the blue image ({dot})");
}

#[test]
fn tags_can_be_turned_off() {
    let dir = tempfile::tempdir().unwrap();
    let red = solid(dir.path(), "red.png", [255, 0, 0]);
    let mut command = plugin(Some(&fixture()));
    command.env("ANYTOPDF_CLIP_TAGS", "off");
    let response = exchange(
        command,
        dir.path(),
        graph(vec![unit(1, "visual", Some(&red))]),
    );
    let unit = &response["graph"]["units"][0];
    assert!(unit["metadata"]["clip.embedding"].is_string());
    assert!(tags(unit).is_empty());
}
