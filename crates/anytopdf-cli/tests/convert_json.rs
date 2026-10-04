use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anytopdf_core::schema;
use anytopdf_pdf::read_embedded_files;
use serde_json::{Value, json};

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

fn validation_errors(instance: &Value) -> Vec<String> {
    let schema = load_schema("convert");
    assert!(
        schema.is_object(),
        "schemas/convert.schema.json must exist and be a JSON object"
    );
    schema::validate(&schema, instance)
        .into_iter()
        .map(|error| format!("{}: {}", error.path, error.message))
        .collect()
}

fn assert_valid(instance: &Value) {
    let errors = validation_errors(instance);
    assert!(errors.is_empty(), "convert schema errors: {errors:?}");
}

fn single_document(output: &Output) -> Value {
    let documents: Vec<Value> = serde_json::Deserializer::from_slice(&output.stdout)
        .into_iter::<Value>()
        .map(|item| {
            item.unwrap_or_else(|e| {
                panic!(
                    "stdout must be JSON only ({e}): {:?} / stderr: {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
            })
        })
        .collect();
    assert_eq!(documents.len(), 1, "stdout must hold exactly one document");
    documents.into_iter().next().unwrap()
}

fn run(dir: &Path, inputs: &[&str], extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .current_dir(dir)
        .arg("--no-plugins")
        .env("PATH", "")
        .env_remove("SOURCE_DATE_EPOCH")
        .arg("convert")
        .args(inputs)
        .args(["--ocr", "off", "--json"])
        .args(extra)
        .output()
        .unwrap()
}

fn mixed(dir: &Path) {
    fs::write(dir.join("valid.txt"), "valid notes").unwrap();
    fs::write(dir.join("invalid.bin"), [0u8; 16]).unwrap();
}

fn collect_strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| collect_strings(v, out)),
        Value::Object(o) => o.values().for_each(|v| collect_strings(v, out)),
        _ => {}
    }
}

#[test]
fn convert_json_success_payload_validates() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("notes.txt"),
        "First page of notes\n\u{c}Second page of notes\n",
    )
    .unwrap();
    let pdf = tmp.path().join("out.pdf");
    let result = run(tmp.path(), &["notes.txt"], &["-o", pdf.to_str().unwrap()]);
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout).to_string();
    assert!(
        !stdout.contains("Wrote"),
        "stdout must be JSON only: {stdout}"
    );
    let payload = single_document(&result);
    assert_valid(&payload);
    assert_eq!(payload["schema_version"], "anytopdf.convert/1");
    assert_eq!(payload["status"], "ok");
    assert_eq!(payload["exit_code"], 0);
    assert!(payload["outputs"][0]["pages"].as_u64().unwrap_or(0) >= 2);
    let files = read_embedded_files(&fs::read(&pdf).unwrap()).unwrap();
    let manifest: Value = serde_json::from_slice(
        &files
            .iter()
            .find(|f| f.name == "anytopdf-manifest.json")
            .expect("manifest attachment")
            .bytes,
    )
    .unwrap();
    assert_eq!(
        payload["summary"]["converted"][0]["sha256"],
        manifest["sources"][0]["sha256"]
    );
    assert_eq!(payload["summary"]["skipped"], json!([]));
}

#[test]
fn convert_json_partial_payload_lists_skipped_inputs_with_codes() {
    let tmp = tempfile::tempdir().unwrap();
    mixed(tmp.path());
    let pdf = tmp.path().join("out.pdf");
    let result = run(
        tmp.path(),
        &["valid.txt", "invalid.bin"],
        &["-o", pdf.to_str().unwrap()],
    );
    assert_eq!(result.status.code(), Some(0));
    let payload = single_document(&result);
    assert_valid(&payload);
    assert_eq!(payload["status"], "partial");
    assert_eq!(payload["exit_code"], 0);
    assert_eq!(
        payload["summary"]["skipped"][0]["code"],
        "input.unsupported"
    );
    assert!(
        payload["summary"]["skipped"][0]["input"]
            .as_str()
            .unwrap_or("")
            .ends_with("invalid.bin")
    );
    assert_eq!(payload["outputs"].as_array().map(Vec::len), Some(1));

    let ff_pdf = tmp.path().join("ff.pdf");
    let result = run(
        tmp.path(),
        &["valid.txt", "invalid.bin"],
        &["--fail-fast", "-o", ff_pdf.to_str().unwrap()],
    );
    assert_eq!(result.status.code(), Some(7));
    let payload = single_document(&result);
    assert_valid(&payload);
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["exit_code"], 7);
    assert_eq!(payload["outputs"], json!([]));
}

#[test]
fn convert_json_strict_failure_still_emits_one_document() {
    let tmp = tempfile::tempdir().unwrap();
    mixed(tmp.path());
    let pdf = tmp.path().join("out.pdf");
    let result = run(
        tmp.path(),
        &["valid.txt", "invalid.bin"],
        &["--strict", "-o", pdf.to_str().unwrap()],
    );
    assert_eq!(result.status.code(), Some(5));
    let payload = single_document(&result);
    assert_valid(&payload);
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["exit_code"], 5);
    assert_eq!(payload["outputs"], json!([]));
    assert!(!pdf.exists());
}

#[test]
fn convert_json_share_profile_reports_base_names_only() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("notes.txt"), "Share notes\n").unwrap();
    let pdf = tmp.path().join("out.pdf");
    let result = run(
        tmp.path(),
        &["notes.txt"],
        &["--profile", "share", "-o", pdf.to_str().unwrap()],
    );
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let payload = single_document(&result);
    assert_valid(&payload);
    assert_eq!(payload["summary"]["converted"][0]["input"], "notes.txt");
    let raw = tmp.path().to_string_lossy().to_string();
    let canonical = fs::canonicalize(tmp.path())
        .unwrap()
        .to_string_lossy()
        .to_string();
    let mut strings = Vec::new();
    collect_strings(&payload, &mut strings);
    for s in strings {
        assert!(
            !s.contains(&raw) && !s.contains(&canonical),
            "share payload leaks temp dir: {s}"
        );
    }
}

#[test]
fn mutated_convert_payload_is_rejected() {
    let valid = json!({
        "schema_version": "anytopdf.convert/1",
        "status": "ok",
        "exit_code": 0,
        "profile": "archive",
        "outputs": [{"path": "out.pdf", "pages": 1, "sha256": "0".repeat(64)}],
        "summary": {
            "converted": [{"input": "a.txt", "source_id": "src-1", "sha256": "0".repeat(64)}],
            "skipped": []
        },
        "diagnostics": []
    });
    assert_valid(&valid);
    let mut bad_status = valid.clone();
    bad_status["status"] = json!("maybe");
    assert!(!validation_errors(&bad_status).is_empty(), "status maybe");
    let mut no_summary = valid.clone();
    no_summary.as_object_mut().unwrap().remove("summary");
    assert!(!validation_errors(&no_summary).is_empty(), "no summary");
}
