use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use anytopdf_core::{DiagnosticCode, schema};
use serde_json::{Value, json};

fn schemas_dir() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.ancestors()
        .map(|dir| dir.join("schemas"))
        .find(|candidate| candidate.is_dir())
        .expect("schemas/ directory must exist")
}

fn load_schema() -> Value {
    let path = schemas_dir().join("capabilities.schema.json");
    assert!(
        path.is_file(),
        "schemas/capabilities.schema.json must exist"
    );
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn validation_errors(instance: &Value) -> Vec<String> {
    schema::validate(&load_schema(), instance)
        .into_iter()
        .map(|error| format!("{}: {}", error.path, error.message))
        .collect()
}

fn run_capabilities() -> Output {
    Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .args(["--no-plugins", "capabilities", "--json"])
        .output()
        .unwrap()
}

fn payload() -> Value {
    let output = run_capabilities();
    assert_eq!(
        output.status.code(),
        Some(0),
        "capabilities --json must exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let documents: Vec<Value> = serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<Value>()
        .map(|item| item.expect("stdout must be JSON only"))
        .collect();
    assert_eq!(documents.len(), 1, "stdout must hold exactly one document");
    documents.into_iter().next().unwrap()
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .expect("expected a JSON array")
        .iter()
        .map(|item| item.as_str().expect("expected a string").to_string())
        .collect()
}

#[test]
fn capabilities_json_validates_against_schema() {
    let document = payload();
    assert_eq!(document["schema_version"], "anytopdf.capabilities/1");
    let errors = validation_errors(&document);
    assert!(errors.is_empty(), "capabilities schema errors: {errors:?}");
}

#[test]
fn capabilities_lists_every_exit_code_and_diagnostic_code() {
    let document = payload();
    let exits: BTreeSet<(i64, String)> = document["exit_codes"]
        .as_array()
        .expect("exit_codes must be an array")
        .iter()
        .map(|entry| {
            (
                entry["code"].as_i64().expect("exit code number"),
                entry["name"].as_str().expect("exit code name").to_string(),
            )
        })
        .collect();
    let expected: BTreeSet<(i64, String)> = [
        (0, "success"),
        (1, "internal"),
        (2, "usage"),
        (3, "input"),
        (4, "provider"),
        (5, "strict"),
        (6, "render"),
        (7, "fail-fast"),
    ]
    .into_iter()
    .map(|(code, name)| (code, name.to_string()))
    .collect();
    assert_eq!(exits, expected);

    let listed: BTreeSet<(String, String)> = document["diagnostic_codes"]
        .as_array()
        .expect("diagnostic_codes must be an array")
        .iter()
        .map(|entry| {
            (
                entry["code"].as_str().expect("diagnostic code").to_string(),
                entry["severity"].as_str().expect("severity").to_string(),
            )
        })
        .collect();
    let wanted: BTreeSet<(String, String)> = DiagnosticCode::ALL
        .iter()
        .map(|code| {
            (
                code.as_str().to_string(),
                format!("{:?}", code.severity()).to_lowercase(),
            )
        })
        .collect();
    assert_eq!(listed, wanted);
}

#[test]
fn capabilities_lists_profiles_ocr_modes_importers_and_schema_ids() {
    let document = payload();
    assert_eq!(strings(&document["profiles"]), ["archive", "share"]);
    assert_eq!(strings(&document["ocr_modes"]).len(), 5);

    let importers: BTreeSet<String> = document["importers"]
        .as_array()
        .expect("importers must be an array")
        .iter()
        .map(|entry| entry["name"].as_str().expect("importer name").to_string())
        .collect();
    for name in ["text", "image", "subtitle", "audio", "ffmpeg-video"] {
        assert!(
            importers.contains(name),
            "missing importer {name}: {importers:?}"
        );
    }

    let on_disk: BTreeSet<String> = fs::read_dir(schemas_dir())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".schema.json"))
        })
        .map(|path| {
            let schema: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            schema["$id"].as_str().expect("schema $id").to_string()
        })
        .collect();
    assert_eq!(on_disk.len(), 9, "9 schema files expected: {on_disk:?}");
    let listed: BTreeSet<String> = document["schemas"]
        .as_array()
        .expect("schemas must be an array")
        .iter()
        .map(|entry| entry["id"].as_str().expect("schema id").to_string())
        .collect();
    assert_eq!(listed, on_disk);
}

#[test]
fn mutated_capabilities_payload_is_rejected() {
    let valid = payload();
    let errors = validation_errors(&valid);
    assert!(errors.is_empty(), "live payload must validate: {errors:?}");

    let mut missing = valid.clone();
    missing.as_object_mut().unwrap().remove("exit_codes");
    assert!(!validation_errors(&missing).is_empty(), "no exit_codes");

    let mut zero = valid.clone();
    zero["exit_codes"][0]["code"] = json!("zero");
    assert!(!validation_errors(&zero).is_empty(), "code zero");
}
