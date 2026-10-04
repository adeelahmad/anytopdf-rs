use std::{fs, path::PathBuf, process::Command};

use anytopdf_core::schema;
use serde_json::{Value, json};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    command.arg("--no-plugins").env("PATH", "");
    command
}

fn load_schema(name: &str) -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root
        .ancestors()
        .map(|dir| dir.join("schemas").join(format!("{name}.schema.json")))
        .find(|candidate| candidate.is_file());
    match path {
        Some(path) => serde_json::from_slice(&fs::read(path).unwrap()).unwrap(),
        None => Value::Null,
    }
}

fn validation_errors(name: &str, instance: &Value) -> Vec<String> {
    let schema = load_schema(name);
    assert!(
        schema.is_object(),
        "schemas/{name}.schema.json must exist and be a JSON object"
    );
    schema::validate(&schema, instance)
        .into_iter()
        .map(|error| format!("{}: {}", error.path, error.message))
        .collect()
}

fn assert_valid(name: &str, instance: &Value) {
    let errors = validation_errors(name, instance);
    assert!(errors.is_empty(), "{name} schema errors: {errors:?}");
}

fn assert_invalid(name: &str, label: &str, instance: &Value) {
    let errors = validation_errors(name, instance);
    assert!(!errors.is_empty(), "{name}: {label} must be rejected");
}

fn run_ok(args: &[&str]) -> Vec<u8> {
    let result = command().args(args).output().unwrap();
    assert!(
        result.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}

fn single_document(stdout: &[u8]) -> Value {
    let documents: Vec<Value> = serde_json::Deserializer::from_slice(stdout)
        .into_iter::<Value>()
        .map(|item| item.expect("stdout must be JSON only"))
        .collect();
    assert_eq!(documents.len(), 1, "stdout must hold exactly one document");
    documents.into_iter().next().unwrap()
}

fn movie() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("movie.mp4");
    fs::write(&source, "not actually a video").unwrap();
    let path = source.to_str().unwrap().to_string();
    (dir, path)
}

#[test]
fn probe_json_is_versioned_and_schema_valid() {
    let (_dir, source) = movie();
    for args in [vec!["probe", "--json", &source], vec!["probe", &source]] {
        let probe = single_document(&run_ok(&args));
        assert_eq!(probe["schema_version"], "anytopdf.probe/1", "{args:?}");
        assert_eq!(probe["importer"]["name"], "ffmpeg-video", "{args:?}");
        assert_valid("probe", &probe);
    }
}

#[test]
fn doctor_json_reports_providers_and_validates() {
    let doctor = single_document(&run_ok(&["doctor", "--json"]));
    assert_valid("doctor", &doctor);
    let providers = doctor["providers"].as_array().expect("providers array");
    let names: Vec<&str> = providers
        .iter()
        .map(|p| p["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        names,
        ["ffmpeg", "ffprobe", "exiftool", "tesseract", "python3"]
    );
    assert!(providers.iter().all(|p| p["available"] == false));
    let ocr: Vec<&str> = doctor["ocr"]
        .as_array()
        .expect("ocr array")
        .iter()
        .map(|o| o["name"].as_str().unwrap_or_default())
        .collect();
    for expected in ["vision", "doctr", "tesseract"] {
        assert!(ocr.contains(&expected), "ocr lists {expected}: {ocr:?}");
    }
}

#[test]
fn plugins_json_lists_builtin_descriptors_and_validates() {
    let plugins = single_document(&run_ok(&["plugins", "--json"]));
    assert_valid("plugins", &plugins);
    let names: Vec<&str> = plugins["plugins"]
        .as_array()
        .expect("plugins array")
        .iter()
        .map(|p| p["name"].as_str().unwrap_or_default())
        .collect();
    for expected in ["text", "image", "subtitle", "audio", "ffmpeg-video", "pdf"] {
        assert!(
            names.contains(&expected),
            "plugins lists {expected}: {names:?}"
        );
    }
}

#[test]
fn json_mode_writes_exactly_one_document_to_stdout() {
    let (_dir, source) = movie();
    let surfaces = [
        vec!["probe", "--json", source.as_str()],
        vec!["doctor", "--json"],
        vec!["plugins", "--json"],
    ];
    for args in surfaces {
        let stdout = run_ok(&args);
        let document = single_document(&stdout);
        assert!(
            document["schema_version"].is_string(),
            "{args:?} must carry schema_version"
        );
        let text = String::from_utf8_lossy(&stdout);
        assert!(
            !text.contains("WARNING") && !text.contains("INFO"),
            "{args:?}"
        );
    }
}

#[test]
fn mutated_payloads_are_rejected_by_the_schemas() {
    let valid = [
        (
            "probe",
            json!({"schema_version": "anytopdf.probe/1", "source": "a.txt",
                   "importer": {"name": "text"}}),
        ),
        (
            "doctor",
            json!({"schema_version": "anytopdf.doctor/1",
                   "providers": [{"name": "ffmpeg", "available": false,
                                  "path": null, "version": null}],
                   "ocr": [{"name": "vision", "available": false, "detail": "x"}]}),
        ),
        (
            "plugins",
            json!({"schema_version": "anytopdf.plugins/1", "plugins": []}),
        ),
    ];
    for (name, payload) in valid {
        let mut missing = payload.clone();
        missing.as_object_mut().unwrap().remove("schema_version");
        assert_invalid(name, "missing schema_version", &missing);
        let mut wrong = payload.clone();
        wrong["schema_version"] = json!(format!("anytopdf.{name}/0"));
        assert_invalid(name, "schema_version /0", &wrong);
        if name == "doctor" {
            let mut stringly = payload.clone();
            stringly["providers"][0]["available"] = json!("no");
            assert_invalid(name, "string available", &stringly);
        }
    }
}
