use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

const BIN: &str = env!("CARGO_BIN_EXE_anytopdf-plugin-face-id");

fn fixture_model() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tiny-embed.onnx")
        .display()
        .to_string()
}

fn run(workspace: &Path, graph: Value, model: &str) -> Value {
    let request = workspace.join("request.json");
    let response = workspace.join("response.json");
    fs::write(
        &request,
        serde_json::to_vec(&json!({
            "protocol": 1,
            "operation": "graph-enrich",
            "workspace": workspace,
            "graph": graph,
        }))
        .unwrap(),
    )
    .unwrap();
    let status = Command::new(BIN)
        .arg("--anytopdf-request")
        .arg(&request)
        .arg("--anytopdf-response")
        .arg(&response)
        .env("ANYTOPDF_FACE_EMBED_MODEL", model)
        .env("ANYTOPDF_DATA_DIR", workspace.join("data"))
        .status()
        .unwrap();
    assert!(status.success());
    serde_json::from_slice(&fs::read(&response).unwrap()).unwrap()
}

fn face(attributes: Value) -> Value {
    json!({
        "kind": "face", "text": "face", "provider": "faces", "confidence": 0.9,
        "region": {"x": 0.2, "y": 0.2, "width": 0.5, "height": 0.6},
        "time_range": null, "attributes": attributes
    })
}

fn graph(dir: &Path, annotations: Vec<Value>) -> Value {
    let image_path = dir.join("frame.png");
    let image = image::RgbImage::from_fn(200, 200, |x, y| image::Rgb([x as u8, y as u8, 128]));
    image.save(&image_path).unwrap();
    json!({
        "sources": [{"id": "00000000-0000-4000-8000-000000000001", "path": image_path, "detected_type": "image/png", "metadata": {}}],
        "units": [{
            "id": "00000000-0000-4000-8000-000000000002",
            "source_id": "00000000-0000-4000-8000-000000000001",
            "kind": "visual", "visual_path": image_path, "visible_text": null, "time_range": null,
            "annotations": annotations, "metadata": {}
        }],
        "metadata": {}
    })
}

const LEFT: &str = "0.30,0.40,0.50,0.40,0.40,0.55,0.32,0.70,0.48,0.70";
// The anytopdf-plugin-faces format: JSON pairs.
const RIGHT: &str = "[[0.55,0.40],[0.75,0.40],[0.65,0.55],[0.57,0.70],[0.73,0.70]]";

#[test]
fn manifest_runs_after_unit_enrichers() {
    let out = Command::new(BIN)
        .arg("--anytopdf-manifest")
        .output()
        .unwrap();
    let manifest: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(manifest["name"], "face-id");
    assert_eq!(manifest["capabilities"][0]["kind"], "graph-enricher");
    assert_eq!(manifest["capabilities"][0]["phase"], "after-units");
}

#[test]
fn faces_with_landmarks_get_references_and_workspace_embeddings() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside.png");
    image::RgbImage::new(112, 112).save(&outside).unwrap();
    let workspace = dir.path().join("job");
    fs::create_dir_all(workspace.join("faces/u1")).unwrap();
    image::RgbImage::from_pixel(112, 112, image::Rgb([200, 30, 30]))
        .save(workspace.join("faces/u1/face-0.png"))
        .unwrap();
    let g = graph(
        dir.path(),
        vec![
            face(json!({"landmarks": LEFT})),
            face(json!({"landmarks": RIGHT})),
            face(json!({})),
            face(json!({"crop": outside})),
            json!({"kind": "ocr", "text": "x", "provider": "ocr", "confidence": null,
                   "region": null, "time_range": null, "attributes": {"landmarks": LEFT}}),
            face(json!({"crop": "faces/u1/face-0.png", "landmarks": LEFT})),
        ],
    );
    let response = run(&workspace, g, &fixture_model());
    assert_eq!(response["ok"], true, "{response}");
    let warnings = response["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0]
            .as_str()
            .unwrap()
            .contains("1 face(s) not embedded: face crop lies outside the job workspace"),
        "{warnings:?}"
    );

    let annotations = &response["graph"]["units"][0]["annotations"];
    let refs: Vec<Option<&str>> = (0..6)
        .map(|i| annotations[i]["attributes"]["face.ref"].as_str())
        .collect();
    assert!(refs[0].is_some() && refs[1].is_some());
    assert_eq!(refs[2..5], [None, None, None]);
    assert!(
        refs[5].is_some(),
        "a relative crop inside the workspace is used"
    );

    let embedded = anytopdf_faces::workspace::read_all(&workspace).unwrap();
    assert_eq!(embedded.len(), 3);
    let a = &embedded[refs[0].unwrap()];
    let b = &embedded[refs[1].unwrap()];
    assert_eq!(a.model.len(), 16);
    assert_eq!(a.vector.len(), 4);
    let norm: f32 = a.vector.iter().map(|v| v * v).sum::<f32>().sqrt();
    assert!((norm - 1.0).abs() < 1e-4);
    assert_ne!(a.vector, b.vector, "different faces must embed differently");
}

#[test]
fn missing_model_is_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let g = graph(dir.path(), vec![face(json!({"landmarks": LEFT}))]);
    let missing = dir.path().join("none.onnx").display().to_string();
    let response = run(dir.path(), g, &missing);
    assert_eq!(response["ok"], true);
    assert!(response["graph"].is_null());
    assert!(
        response["warnings"][0]
            .as_str()
            .unwrap()
            .contains("no face embedding model")
    );
}

#[test]
fn graphs_without_faces_need_no_model() {
    let dir = tempfile::tempdir().unwrap();
    let g = graph(dir.path(), vec![]);
    let response = run(dir.path(), g, "");
    assert_eq!(response["ok"], true);
    assert_eq!(response["warnings"], json!([]));
    assert!(response["graph"].is_null());
}

/// Regression for 0.3.0, where every crop was scaled to -1..1 even for models
/// that scale their own input (SFace, ONNX-zoo ArcFace): the model then saw a
/// near-black image for every face, so different people scored 0.94 to 0.98.
#[test]
fn different_faces_score_low_with_a_model_that_scales_its_own_input() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("job");
    fs::create_dir_all(workspace.join("faces")).unwrap();
    let light_left = |x: u32, _: u32, boost: u8| if x < 56 { 220 + boost } else { 40 + boost };
    let light_top = |_: u32, y: u32, _: u8| if y < 56 { 220 } else { 40 };
    for (name, shade) in [
        ("a.png", &light_left as &dyn Fn(u32, u32, u8) -> u8),
        ("b.png", &light_top),
    ] {
        image::RgbImage::from_fn(112, 112, |x, y| image::Rgb([shade(x, y, 0); 3]))
            .save(workspace.join("faces").join(name))
            .unwrap();
    }
    // The same "person" again, a little brighter.
    image::RgbImage::from_fn(112, 112, |x, y| image::Rgb([light_left(x, y, 15); 3]))
        .save(workspace.join("faces/a2.png"))
        .unwrap();
    let g = graph(
        dir.path(),
        ["a", "b", "a2"]
            .iter()
            .map(|n| face(json!({"crop": format!("faces/{n}.png")})))
            .collect(),
    );
    let model = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/tiny-embed-raw.onnx")
        .display()
        .to_string();
    let response = run(&workspace, g, &model);
    assert_eq!(response["warnings"], json!([]), "{response}");
    let embedded = anytopdf_faces::workspace::read_all(&workspace).unwrap();
    let vector = |i: usize| {
        let face_ref = response["graph"]["units"][0]["annotations"][i]["attributes"]["face.ref"]
            .as_str()
            .unwrap();
        embedded[face_ref].vector.clone()
    };
    let cosine = anytopdf_faces::vector::cosine;
    let (a, b, a2) = (vector(0), vector(1), vector(2));
    assert!(cosine(&a, &b) < 0.2, "different faces: {}", cosine(&a, &b));
    assert!(cosine(&a, &a2) > 0.9, "same face: {}", cosine(&a, &a2));
}
