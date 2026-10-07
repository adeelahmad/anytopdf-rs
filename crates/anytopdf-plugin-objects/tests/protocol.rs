use anytopdf_onnx::testing;
use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

const ENV: [&str; 8] = [
    "ANYTOPDF_OBJECTS_MODEL",
    "ANYTOPDF_OBJECTS_LABELS",
    "ANYTOPDF_OBJECTS_CONFIDENCE",
    "ANYTOPDF_OBJECTS_IOU",
    "ANYTOPDF_OBJECTS_CLASSES",
    "ANYTOPDF_OBJECTS_INPUT_SIZE",
    "ANYTOPDF_OBJECTS_MAX",
    "ANYTOPDF_OBJECTS_DEVICE",
];

fn plugin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf-plugin-objects"));
    for name in ENV {
        command.env_remove(name);
    }
    command
}

const SOURCE: &str = "11111111-1111-4111-8111-111111111111";

fn frame_graph(frame: &Path) -> Value {
    json!({
        "sources": [{"id": SOURCE, "path": frame, "detected_type": "video/mp4", "metadata": {}}],
        "units": [{"id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "source_id": SOURCE,
                   "kind": "visual", "visual_path": frame, "visible_text": null,
                   "time_range": {"start_seconds": 12.5, "end_seconds": 12.5},
                   "annotations": [], "metadata": {}}],
        "metadata": {}
    })
}

/// A 64x32 frame and a 32x32 fixture model whose head always reports a cat
/// box (twice, overlapping) and a weaker dog box: `[1, 4 + 2 classes, 3]`.
fn fixture(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let frame = dir.join("frame.png");
    image::RgbImage::from_pixel(64, 32, image::Rgb([40, 80, 120]))
        .save(&frame)
        .unwrap();
    #[rustfmt::skip]
    let head = vec![
        16.0, 17.0, 6.0,   // cx
        16.0, 16.0, 12.0,  // cy
        16.0, 16.0, 4.0,   // w
        8.0,  8.0,  4.0,   // h
        0.9,  0.8,  0.1,   // cat
        0.05, 0.1,  0.6,   // dog
    ];
    let model = dir.join("tiny-yolo.onnx");
    let proto = testing::constant_model(
        Some(32),
        &[1, 6, 3],
        head,
        &[("names", "{0: 'cat', 1: 'dog'}")],
    );
    fs::write(&model, testing::encode(&proto)).unwrap();
    (frame, model)
}

fn exchange(command: &mut Command, workspace: &Path, graph: Value) -> Value {
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

#[test]
fn manifest_declares_a_protocol_v1_graph_enricher_for_images_and_video() {
    let output = plugin().arg("--anytopdf-manifest").output().unwrap();
    assert!(output.status.success());
    let manifest: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(manifest["protocol"], 1);
    assert_eq!(manifest["name"], "objects");
    assert_eq!(manifest["capabilities"][0]["kind"], "graph-enricher");
    assert_eq!(
        manifest["capabilities"][0]["mime_types"],
        json!(["image/*", "video/*"])
    );
}

#[test]
fn graphs_without_frames_come_back_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let graph = json!({"sources": [], "units": [], "metadata": {}});
    let response = exchange(&mut plugin(), dir.path(), graph);
    assert_eq!(response["ok"], true);
    assert_eq!(response["warnings"], json!([]));
    assert!(response.get("graph").is_none());
}

#[test]
fn missing_model_is_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (frame, _) = fixture(dir.path());
    let response = exchange(&mut plugin(), dir.path(), frame_graph(&frame));
    assert_eq!(response["ok"], true);
    assert!(response.get("graph").is_none());
    let warning = response["warnings"][0].as_str().unwrap();
    assert!(warning.contains("ANYTOPDF_OBJECTS_MODEL"), "{warning}");
    assert!(warning.contains("1 image(s) not analysed"), "{warning}");
}

#[test]
fn detections_become_located_timed_object_annotations_the_host_accepts() {
    let dir = tempfile::tempdir().unwrap();
    let (frame, model) = fixture(dir.path());
    let mut command = plugin();
    command
        .env("ANYTOPDF_OBJECTS_MODEL", &model)
        .env("ANYTOPDF_OBJECTS_DEVICE", "cuda");
    let response = exchange(&mut command, dir.path(), frame_graph(&frame));
    assert_eq!(response["ok"], true);
    assert_eq!(response["warnings"].as_array().unwrap().len(), 1);
    assert!(response["warnings"][0].as_str().unwrap().contains("CPU"));

    let annotations = response["graph"]["units"][0]["annotations"]
        .as_array()
        .unwrap();
    // The overlapping cat box is suppressed; cat, dog and the count remain.
    let texts: Vec<&str> = annotations
        .iter()
        .map(|a| a["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["cat", "dog", "objects: 1 cat, 1 dog"]);
    let cat = &annotations[0];
    assert_eq!(cat["kind"], "object");
    assert_eq!(cat["provider"], "yolo");
    assert!((cat["confidence"].as_f64().unwrap() - 0.9).abs() < 1e-6);
    assert_eq!(cat["time_range"]["start_seconds"], 12.5);
    assert_eq!(cat["attributes"]["model"], "tiny-yolo.onnx");
    // 64x32 frame letterboxed into 32x32: scale 0.5, 8 px of padding on top
    // and bottom. The 16x8 box centred at (16, 16) covers the middle half.
    let region = &cat["region"];
    for (key, expected) in [("x", 0.25), ("y", 0.25), ("width", 0.5), ("height", 0.5)] {
        let value = region[key].as_f64().unwrap();
        assert!((value - expected).abs() < 1e-5, "{key} = {value}");
    }

    let graph: anytopdf_core::DocumentGraph =
        serde_json::from_value(response["graph"].clone()).unwrap();
    graph.validate().unwrap();
    assert_eq!(
        graph.units[0].annotations[0].kind,
        anytopdf_core::AnnotationKind::Object
    );
}

#[test]
fn class_allow_list_and_labels_file_are_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let (frame, model) = fixture(dir.path());
    let labels = dir.path().join("labels.txt");
    fs::write(&labels, "kitten\npuppy\n").unwrap();
    let mut command = plugin();
    command
        .env("ANYTOPDF_OBJECTS_MODEL", &model)
        .env("ANYTOPDF_OBJECTS_LABELS", &labels)
        .env("ANYTOPDF_OBJECTS_CLASSES", "Puppy");
    let response = exchange(&mut command, dir.path(), frame_graph(&frame));
    let texts: Vec<&str> = response["graph"]["units"][0]["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, ["puppy", "objects: 1 puppy"]);
}

#[test]
fn unreadable_frames_and_bad_settings_are_warnings() {
    let dir = tempfile::tempdir().unwrap();
    let (_, model) = fixture(dir.path());
    let broken = dir.path().join("broken.png");
    fs::write(&broken, b"not an image").unwrap();
    let mut command = plugin();
    command.env("ANYTOPDF_OBJECTS_MODEL", &model);
    let response = exchange(&mut command, dir.path(), frame_graph(&broken));
    assert_eq!(response["ok"], true);
    assert!(response.get("graph").is_none());
    assert!(
        response["warnings"][0]
            .as_str()
            .unwrap()
            .starts_with("objects: broken.png:")
    );

    let mut command = plugin();
    command
        .env("ANYTOPDF_OBJECTS_MODEL", &model)
        .env("ANYTOPDF_OBJECTS_CLASSES", "giraffe");
    let response = exchange(&mut command, dir.path(), frame_graph(&broken));
    assert!(
        response["warnings"][0]
            .as_str()
            .unwrap()
            .contains("\"giraffe\"")
    );
}

/// A 32x32 fixture model with `names` cat and dog whose output is `values`
/// in `shape`.
fn model_with_head(dir: &Path, shape: &[i64], values: Vec<f32>) -> std::path::PathBuf {
    let model = dir.join("head.onnx");
    let proto = testing::constant_model(
        Some(32),
        shape,
        values,
        &[("names", "{0: 'cat', 1: 'dog'}")],
    );
    fs::write(&model, testing::encode(&proto)).unwrap();
    model
}

#[test]
fn yolox_models_find_objects() {
    let dir = tempfile::tempdir().unwrap();
    let frame = dir.path().join("frame.png");
    image::RgbImage::from_pixel(32, 32, image::Rgb([40, 80, 120]))
        .save(&frame)
        .unwrap();
    // A raw YOLOX head for a 32x32 input: 4x4 + 2x2 + 1x1 grid rows, each
    // `x, y, log w, log h, objectness, cat, dog`. Row 5 is the stride-8 cell
    // (1, 1): an 8x8 cat box at (8, 8).
    let mut head = vec![0.0; 21 * 7];
    head[5 * 7..6 * 7].copy_from_slice(&[0.5, 0.5, 0.0, 0.0, 0.9, 0.95, 0.1]);
    let model = model_with_head(dir.path(), &[1, 21, 7], head);
    let mut command = plugin();
    command.env("ANYTOPDF_OBJECTS_MODEL", &model);
    let response = exchange(&mut command, dir.path(), frame_graph(&frame));
    assert_eq!(response["warnings"], json!([]));
    let cat = &response["graph"]["units"][0]["annotations"][0];
    assert_eq!(cat["text"], "cat");
    let region = &cat["region"];
    for (key, expected) in [("x", 0.25), ("y", 0.25), ("width", 0.25), ("height", 0.25)] {
        let value = region[key].as_f64().unwrap();
        assert!((value - expected).abs() < 1e-5, "{key} = {value}");
    }
}

#[test]
fn unrecognised_models_are_reported_instead_of_finding_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (frame, _) = fixture(dir.path());
    // Nine features per box fits neither 2 + 4 nor 2 + 5.
    let model = model_with_head(dir.path(), &[1, 9, 3], vec![0.5; 27]);
    let mut command = plugin();
    command.env("ANYTOPDF_OBJECTS_MODEL", &model);
    let response = exchange(&mut command, dir.path(), frame_graph(&frame));
    assert_eq!(response["ok"], true);
    assert!(response.get("graph").is_none());
    let warning = response["warnings"][0].as_str().unwrap();
    assert!(
        warning.contains("head.onnx is not a recognised YOLO detector"),
        "{warning}"
    );
    assert!(warning.contains("1 image(s) not analysed"), "{warning}");
}
