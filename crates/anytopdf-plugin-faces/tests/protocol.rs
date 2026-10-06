use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

const ASTRONAUT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/astronaut.jpg");

fn plugin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf-plugin-faces"));
    for name in [
        "ANYTOPDF_FACES",
        "ANYTOPDF_FACES_THRESHOLD",
        "ANYTOPDF_FACES_MIN_SIZE",
        "ANYTOPDF_FACES_INPUT_SIZE",
        "ANYTOPDF_FACES_MODEL",
        "ANYTOPDF_FACES_CROPS",
    ] {
        command.env_remove(name);
    }
    command
}

fn visual_unit(path: &Path) -> Value {
    json!({
        "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "source_id": "11111111-1111-4111-8111-111111111111",
        "kind": "visual", "visual_path": path, "visible_text": null,
        "time_range": {"start_seconds": 12.5, "end_seconds": 12.5},
        "annotations": [], "metadata": {}
    })
}

fn exchange(command: &mut Command, workspace: &Path, operation: &str, unit: Value) -> Value {
    let request = workspace.join("request.json");
    let response = workspace.join("response.json");
    fs::write(
        &request,
        serde_json::to_vec(&json!({
            "protocol": 1, "operation": operation, "workspace": workspace,
            "source": {"id": "11111111-1111-4111-8111-111111111111", "path": "/in/clip.mp4",
                       "detected_type": "video/mp4", "metadata": {}},
            "unit": unit, "graph": null, "output": null,
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
fn manifest_declares_a_protocol_v1_unit_enricher_for_images_and_video() {
    let output = plugin().arg("--anytopdf-manifest").output().unwrap();
    assert!(output.status.success());
    let manifest: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(manifest["protocol"], 1);
    assert_eq!(manifest["name"], "faces");
    let capability = &manifest["capabilities"][0];
    assert_eq!(capability["kind"], "unit-enricher");
    assert_eq!(capability["mime_types"], json!(["image/*", "video/*"]));
}

#[test]
fn portrait_yields_one_located_face_a_count_and_an_aligned_crop() {
    let dir = tempfile::tempdir().unwrap();
    let response = exchange(
        &mut plugin(),
        dir.path(),
        "unit-enrich",
        visual_unit(Path::new(ASTRONAUT)),
    );
    assert_eq!(response["ok"], true, "{response}");
    let annotations = response["annotations"].as_array().unwrap();
    assert_eq!(annotations.len(), 2, "{response}");
    let face = &annotations[0];
    assert_eq!(face["kind"], "face");
    assert_eq!(face["provider"], "yunet");
    assert!(face["confidence"].as_f64().unwrap() >= 0.8);
    assert_eq!(face["time_range"]["start_seconds"], 12.5);
    // The face sits in the upper middle of the portrait.
    let region = &face["region"];
    let center_x = region["x"].as_f64().unwrap() + region["width"].as_f64().unwrap() / 2.0;
    let center_y = region["y"].as_f64().unwrap() + region["height"].as_f64().unwrap() / 2.0;
    assert!((0.3..0.7).contains(&center_x), "{region}");
    assert!((0.1..0.5).contains(&center_y), "{region}");
    let landmarks: Vec<[f64; 2]> =
        serde_json::from_str(face["attributes"]["landmarks"].as_str().unwrap()).unwrap();
    assert_eq!(landmarks.len(), 5);
    // The subject's right eye appears left of their left eye.
    assert!(landmarks[0][0] < landmarks[1][0]);
    let crop = dir
        .path()
        .join(face["attributes"]["crop"].as_str().unwrap());
    let crop = image::open(crop).unwrap();
    assert_eq!((crop.width(), crop.height()), (112, 112));
    assert_eq!(annotations[1]["text"], "1 face");
    assert_eq!(annotations[1]["attributes"]["face_count"], "1");
}

#[test]
fn blank_frame_and_non_visual_units_get_no_annotations() {
    let dir = tempfile::tempdir().unwrap();
    let blank = dir.path().join("blank.png");
    image::RgbImage::from_pixel(120, 80, image::Rgb([200, 200, 200]))
        .save(&blank)
        .unwrap();
    let response = exchange(
        &mut plugin(),
        dir.path(),
        "unit-enrich",
        visual_unit(&blank),
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["annotations"], json!([]));

    let mut text = visual_unit(Path::new(ASTRONAUT));
    text["kind"] = json!("text");
    let response = exchange(&mut plugin(), dir.path(), "unit-enrich", text);
    assert_eq!(response["annotations"], json!([]));
    assert!(!dir.path().join("faces").exists());
}

#[test]
fn faces_off_and_crops_off_are_honoured() {
    let dir = tempfile::tempdir().unwrap();
    let mut off = plugin();
    off.env("ANYTOPDF_FACES", "off");
    let response = exchange(
        &mut off,
        dir.path(),
        "unit-enrich",
        visual_unit(Path::new(ASTRONAUT)),
    );
    assert_eq!(response["annotations"], json!([]));

    let mut no_crops = plugin();
    no_crops.env("ANYTOPDF_FACES_CROPS", "off");
    let response = exchange(
        &mut no_crops,
        dir.path(),
        "unit-enrich",
        visual_unit(Path::new(ASTRONAUT)),
    );
    assert_eq!(response["annotations"].as_array().unwrap().len(), 2);
    assert!(
        response["annotations"][0]["attributes"]
            .get("crop")
            .is_none()
    );
    assert!(!dir.path().join("faces").exists());
}

#[test]
fn min_size_filters_small_faces() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = plugin();
    command.env("ANYTOPDF_FACES_MIN_SIZE", "500");
    let response = exchange(
        &mut command,
        dir.path(),
        "unit-enrich",
        visual_unit(Path::new(ASTRONAUT)),
    );
    assert_eq!(response["annotations"], json!([]));
}

#[test]
fn failures_are_reported_as_errors_not_crashes() {
    let dir = tempfile::tempdir().unwrap();
    let missing = exchange(
        &mut plugin(),
        dir.path(),
        "unit-enrich",
        visual_unit(&dir.path().join("missing.png")),
    );
    assert_eq!(missing["ok"], false);
    assert!(missing["error"].as_str().unwrap().contains("missing.png"));

    let mut bad = plugin();
    bad.env("ANYTOPDF_FACES_THRESHOLD", "2");
    let response = exchange(
        &mut bad,
        dir.path(),
        "unit-enrich",
        visual_unit(Path::new(ASTRONAUT)),
    );
    assert_eq!(response["ok"], false);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("ANYTOPDF_FACES_THRESHOLD")
    );

    let wrong = exchange(
        &mut plugin(),
        dir.path(),
        "import",
        visual_unit(Path::new(ASTRONAUT)),
    );
    assert_eq!(wrong["ok"], false);
}
