use anytopdf_core::{DocumentGraph, JobContext, Renderer, SourceRecord, Unit, schema};
use anytopdf_pdf::{SearchablePdfRenderer, read_embedded_files};
use serde_json::Value;

#[path = "common/stdout_json.rs"]
mod stdout_json;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use stdout_json::single_document;

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

fn rewrite_sidecar(dir: &Path, kind: &str, value: &Value) {
    fs::write(
        dir.join(format!("plain.pdf.{kind}.json")),
        serde_json::to_vec(value).unwrap(),
    )
    .unwrap();
}

fn nested_array(depth: usize) -> Value {
    let mut value = Value::from(1);
    for _ in 0..depth {
        value = Value::Array(vec![value]);
    }
    value
}

#[test]
fn extract_rejects_manifest_missing_a_required_field() {
    let dir = tempfile::tempdir().unwrap();
    let (plain, mut manifest, _) = sidecar_fixture(dir.path(), |_| {});
    assert_eq!(extract_ok(&plain)["origin"], "sidecar");
    manifest.as_object_mut().unwrap().remove("generator");
    rewrite_sidecar(dir.path(), "manifest", &manifest);
    let out = run_extract(&plain, true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
    assert!(out.stdout.is_empty(), "stdout must be empty");
    assert!(
        stderr.contains("manifest does not match anytopdf.manifest/1"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("`generator`"), "stderr: {stderr}");
    assert!(
        !stderr.contains("[extract.version-mismatch]"),
        "stderr: {stderr}"
    );
    let typed = dir.path().display().to_string();
    let canonical = fs::canonicalize(dir.path()).unwrap().display().to_string();
    let stripped = canonical.strip_prefix(r"\\?\").unwrap_or(&canonical);
    for form in [typed.as_str(), canonical.as_str(), stripped] {
        assert!(!stderr.contains(form), "stderr leaks {form}: {stderr}");
    }
}

#[test]
fn extract_rejects_manifest_with_wrong_anchor_type() {
    let dir = tempfile::tempdir().unwrap();
    let (plain, mut manifest, _) = sidecar_fixture(dir.path(), |_| {});
    assert!(manifest["units"][0]["anchor"].is_object());
    assert_eq!(extract_ok(&plain)["origin"], "sidecar");
    manifest["units"][0]["anchor"] = Value::from("region");
    rewrite_sidecar(dir.path(), "manifest", &manifest);
    let out = run_extract(&plain, true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
    assert!(out.stdout.is_empty(), "stdout must be empty");
    assert!(
        stderr.contains("manifest does not match anytopdf.manifest/1: /units/0/anchor"),
        "stderr: {stderr}"
    );
}

#[test]
fn extract_rejects_chunks_missing_a_required_field() {
    let dir = tempfile::tempdir().unwrap();
    let (plain, _, mut chunks) = sidecar_fixture(dir.path(), |_| {});
    assert_eq!(extract_ok(&plain)["origin"], "sidecar");
    chunks["chunks"][0].as_object_mut().unwrap().remove("text");
    rewrite_sidecar(dir.path(), "chunks", &chunks);
    let out = run_extract(&plain, true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
    assert!(out.stdout.is_empty(), "stdout must be empty");
    assert!(
        stderr.contains("chunks does not match anytopdf.chunks/1: /chunks/0"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("`text`"), "stderr: {stderr}");
}

#[test]
fn extract_reports_how_many_further_schema_errors_exist() {
    let dir = tempfile::tempdir().unwrap();
    let (plain, mut manifest, _) = sidecar_fixture(dir.path(), |_| {});
    assert_eq!(extract_ok(&plain)["origin"], "sidecar");
    let obj = manifest.as_object_mut().unwrap();
    obj.remove("generator");
    obj.remove("profile");
    rewrite_sidecar(dir.path(), "manifest", &manifest);
    let out = run_extract(&plain, true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "stderr: {stderr}");
    assert!(
        stderr.contains("manifest does not match anytopdf.manifest/1"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("(+1 more)"), "stderr: {stderr}");
}

#[test]
fn extract_rejects_deep_nesting_without_crashing() {
    let dir = tempfile::tempdir().unwrap();
    let (plain, mut manifest, _) = sidecar_fixture(dir.path(), |_| {});
    assert_eq!(extract_ok(&plain)["origin"], "sidecar");
    manifest["units"][0]["anchor"] = nested_array(100);
    rewrite_sidecar(dir.path(), "manifest", &manifest);
    let a = run_extract(&plain, true);
    let a_err = String::from_utf8_lossy(&a.stderr);
    assert_eq!(a.status.code(), Some(3), "case A stderr: {a_err}");
    assert!(
        a_err.contains("manifest does not match anytopdf.manifest/1: /units/0/anchor"),
        "case A stderr: {a_err}"
    );
    manifest["units"][0]["anchor"] = nested_array(200);
    rewrite_sidecar(dir.path(), "manifest", &manifest);
    let b = run_extract(&plain, true);
    let b_err = String::from_utf8_lossy(&b.stderr);
    assert_eq!(b.status.code(), Some(3), "case B stderr: {b_err}");
    assert!(!b_err.contains("does not match"), "case B stderr: {b_err}");
}

#[test]
fn extract_skips_schema_validation_on_version_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let (plain, _, _) = sidecar_fixture(dir.path(), |manifest| {
        manifest["schema_version"] = Value::from("anytopdf.manifest/999");
        manifest.as_object_mut().unwrap().remove("generator");
    });
    let out = run_extract(&plain, true);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "stderr: {stderr}");
    let doc = single_document(&out.stdout);
    assert!(
        doc["warnings"]
            .as_array()
            .expect("warnings array")
            .iter()
            .any(|w| w["code"] == "extract.version-mismatch"),
        "warnings: {}",
        doc["warnings"]
    );
    assert!(stderr.contains("[extract.version-mismatch]"), "{stderr}");
    assert!(!stderr.contains("does not match"), "{stderr}");
}

#[test]
fn extract_accepts_share_profile_output_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.txt");
    fs::write(&notes, "Extract fixture line one\nline two\n").unwrap();
    let pdf = dir.path().join("share.pdf");
    let out = anytopdf()
        .arg("convert")
        .arg(&notes)
        .args(["--profile", "share", "--ocr", "off", "-o"])
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "convert failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = extract_ok(&pdf);
    assert_eq!(doc["origin"], "embedded");
    assert_eq!(
        doc["manifest"],
        embedded_json(&pdf, "anytopdf-manifest.json")
    );
    assert_eq!(doc["chunks"], embedded_json(&pdf, "anytopdf-chunks.json"));
    assert_eq!(doc["warnings"], serde_json::json!([]));
}

fn convert_links(dir: &Path, extra: &[&str]) -> Value {
    let notes = dir.join("links.txt");
    fs::write(
        &notes,
        "Docs at https://github.com/adeelahmad/anytopdf-rs.\nMail adeel@example.com by 03/04/2024\n",
    )
    .unwrap();
    let pdf = dir.join("links.pdf");
    let out = anytopdf()
        .arg("convert")
        .arg(&notes)
        .args(["--ocr", "off"])
        .args(extra)
        .arg("-o")
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "convert failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    extract_ok(&pdf)
}

#[test]
fn convert_lists_urls_and_emails_as_chunk_entities() {
    let dir = tempfile::tempdir().unwrap();
    let doc = convert_links(dir.path(), &[]);
    let chunk = &doc["chunks"]["chunks"][0];
    assert_eq!(
        chunk["entities"],
        serde_json::json!([
            {"kind": "url", "value": "https://github.com/adeelahmad/anytopdf-rs"},
            {"kind": "email", "value": "adeel@example.com"},
            {"kind": "date", "value": "2024-04-03"}
        ])
    );
    assert!(
        chunk["providers"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("text-entities"))
    );
    assert!(chunk["text"].as_str().unwrap().starts_with("Docs at"));
}

#[test]
fn date_order_flag_reads_numeric_dates_month_first() {
    let dir = tempfile::tempdir().unwrap();
    let doc = convert_links(dir.path(), &["--date-order", "mdy"]);
    let entities = doc["chunks"]["chunks"][0]["entities"].as_array().unwrap();
    assert_eq!(
        entities.last().unwrap(),
        &serde_json::json!({"kind": "date", "value": "2024-03-04"})
    );
}

#[test]
fn no_entities_flag_leaves_chunks_without_entities() {
    let dir = tempfile::tempdir().unwrap();
    let doc = convert_links(dir.path(), &["--no-entities"]);
    assert!(doc["chunks"]["chunks"][0].get("entities").is_none());
}
