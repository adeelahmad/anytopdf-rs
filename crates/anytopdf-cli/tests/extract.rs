use anytopdf_core::{DocumentGraph, JobContext, Renderer, SourceRecord, Unit, schema};
use anytopdf_pdf::{SearchablePdfRenderer, read_embedded_files};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

fn anytopdf() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    cmd.arg("--no-plugins")
        .env("PATH", "")
        .env_remove("SOURCE_DATE_EPOCH");
    cmd
}

fn convert_notes(dir: &Path) -> PathBuf {
    let notes = dir.join("notes.txt");
    fs::write(&notes, "Extract fixture line one\nline two\n").unwrap();
    let pdf = dir.join("out.pdf");
    let out = anytopdf()
        .arg("convert")
        .arg(&notes)
        .args(["--ocr", "off", "-o"])
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "convert failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    pdf
}

fn embedded_json(pdf: &Path, name: &str) -> Value {
    let files = read_embedded_files(&fs::read(pdf).unwrap()).unwrap();
    let file = files
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("attachment {name} missing in {}", pdf.display()));
    serde_json::from_slice(&file.bytes).unwrap()
}

fn plain_pdf(dir: &Path, name: &str) -> PathBuf {
    let source = SourceRecord::new(PathBuf::from("plain.txt"));
    let graph = DocumentGraph {
        units: vec![Unit::text(source.id, "plain body".into())],
        sources: vec![source],
        ..Default::default()
    };
    let ctx = JobContext {
        workspace: dir.into(),
        quiet: true,
    };
    let pdf = dir.join(name);
    SearchablePdfRenderer::default()
        .render(&ctx, &graph, &pdf)
        .unwrap();
    pdf
}

fn run_extract(pdf: &Path, json: bool) -> Output {
    let mut cmd = anytopdf();
    cmd.arg("extract").arg(pdf);
    if json {
        cmd.arg("--json");
    }
    cmd.output().unwrap()
}

fn single_document(stdout: &[u8]) -> Value {
    let docs: Vec<Value> = serde_json::Deserializer::from_slice(stdout)
        .into_iter::<Value>()
        .map(|item| item.expect("stdout must be JSON only"))
        .collect();
    assert_eq!(docs.len(), 1, "stdout must hold exactly one JSON value");
    docs.into_iter().next().unwrap()
}

fn extract_ok(pdf: &Path) -> Value {
    let out = run_extract(pdf, true);
    assert_eq!(
        out.status.code(),
        Some(0),
        "extract --json failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    single_document(&out.stdout)
}

/// Writes the manifest and chunks of a converted PDF as sidecars of a plain PDF.
fn sidecar_fixture(dir: &Path, mutate: impl FnOnce(&mut Value)) -> (PathBuf, Value, Value) {
    let converted = convert_notes(dir);
    let mut manifest = embedded_json(&converted, "anytopdf-manifest.json");
    let chunks = embedded_json(&converted, "anytopdf-chunks.json");
    mutate(&mut manifest);
    let plain = plain_pdf(dir, "plain.pdf");
    fs::write(
        dir.join("plain.pdf.manifest.json"),
        serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.join("plain.pdf.chunks.json"),
        serde_json::to_vec(&chunks).unwrap(),
    )
    .unwrap();
    (plain, manifest, chunks)
}

fn load_extract_schema() -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root
        .ancestors()
        .map(|dir| dir.join("schemas").join("extract.schema.json"))
        .find(|candidate| candidate.is_file())
        .expect("schemas/extract.schema.json must exist");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn extract_json_returns_embedded_manifest_and_chunks_written_by_convert() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = convert_notes(dir.path());
    let doc = extract_ok(&pdf);
    assert_eq!(doc["origin"], "embedded");
    assert_eq!(
        doc["manifest"],
        embedded_json(&pdf, "anytopdf-manifest.json")
    );
    assert_eq!(doc["chunks"], embedded_json(&pdf, "anytopdf-chunks.json"));
    assert_eq!(doc["warnings"], serde_json::json!([]));
}

#[test]
fn extract_falls_back_to_documented_sidecar_files() {
    let dir = tempfile::tempdir().unwrap();
    let (plain, manifest, chunks) = sidecar_fixture(dir.path(), |_| {});
    let doc = extract_ok(&plain);
    assert_eq!(doc["origin"], "sidecar");
    assert_eq!(doc["manifest"], manifest);
    assert_eq!(doc["chunks"], chunks);
}

#[test]
fn extract_reports_manifest_version_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let (plain, _, _) = sidecar_fixture(dir.path(), |manifest| {
        manifest["schema_version"] = Value::from("anytopdf.manifest/999");
    });
    let out = run_extract(&plain, true);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = single_document(&out.stdout);
    let warnings = doc["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w["code"] == "extract.version-mismatch"),
        "warnings: {warnings:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("[extract.version-mismatch]"),
        "stderr: {stderr}"
    );
}

#[test]
fn extract_json_validates_against_schema_and_rejects_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = convert_notes(dir.path());
    let doc = extract_ok(&pdf);
    let schema_doc = load_extract_schema();
    let errors = schema::validate(&schema_doc, &doc);
    assert!(errors.is_empty(), "extract schema errors: {errors:?}");

    let mut missing = doc.clone();
    missing.as_object_mut().unwrap().remove("origin");
    assert!(
        !schema::validate(&schema_doc, &missing).is_empty(),
        "removing origin must be rejected"
    );
    let mut wrong = doc.clone();
    wrong["origin"] = Value::from("elsewhere");
    assert!(
        !schema::validate(&schema_doc, &wrong).is_empty(),
        "origin=elsewhere must be rejected"
    );
}

#[test]
fn extract_without_manifest_exits_with_input_code() {
    let dir = tempfile::tempdir().unwrap();
    let plain = plain_pdf(dir.path(), "bare.pdf");
    let out = run_extract(&plain, true);
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    if !out.stdout.is_empty() {
        single_document(&out.stdout);
    }
    let absent = run_extract(&dir.path().join("absent.pdf"), true);
    assert_eq!(absent.status.code(), Some(3));
}
