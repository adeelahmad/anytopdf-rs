use anytopdf_core::sha256_hex;
use anytopdf_pdf::{EmbeddedFile, read_embedded_files};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[path = "common/png.rs"]
mod png;
#[path = "common/png_gray.rs"]
mod png_gray;
#[path = "common/schema_assert.rs"]
mod schema_assert;
#[path = "common/schema.rs"]
mod schema_files;
use png_gray::write_png;
use schema_assert::assert_valid;

fn page_count(pdf: &[u8]) -> u64 {
    let text = String::from_utf8_lossy(pdf);
    regex::Regex::new(r"/Type\s*/Page\b")
        .unwrap()
        .find_iter(&text)
        .count() as u64
}

fn convert(inputs: &[&Path], pdf: &Path, extra: &[&str], dump: Option<&Path>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    cmd.arg("--no-plugins")
        .env("PATH", "")
        .env_remove("SOURCE_DATE_EPOCH")
        .arg("convert")
        .args(inputs)
        .args(["--ocr", "off", "-o"])
        .arg(pdf);
    if let Some(dump) = dump {
        cmd.arg("--dump-graph").arg(dump);
    }
    let out = cmd.args(extra).output().unwrap();
    assert!(
        out.status.success(),
        "convert failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn attachments(pdf: &Path) -> Vec<EmbeddedFile> {
    read_embedded_files(&fs::read(pdf).unwrap()).unwrap()
}

fn find<'a>(files: &'a [EmbeddedFile], name: &str) -> &'a EmbeddedFile {
    files
        .iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("attachment {name} missing; found {:?}", names(files)))
}

fn names(files: &[EmbeddedFile]) -> Vec<&str> {
    files.iter().map(|f| f.name.as_str()).collect()
}

fn json_of(file: &EmbeddedFile) -> Value {
    serde_json::from_slice(&file.bytes).unwrap()
}

fn strings(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        Value::Object(o) => o.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

fn setup(dir: &Path) -> PathBuf {
    let notes = dir.join("notes.txt");
    fs::write(&notes, "Identity fixture\n").unwrap();
    notes
}

#[test]
fn converted_pdf_embeds_schema_valid_manifest_and_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = setup(tmp.path());
    let pdf = tmp.path().join("out.pdf");
    convert(&[&notes], &pdf, &[], None);

    let files = attachments(&pdf);
    let mut got = names(&files);
    got.sort();
    assert_eq!(got, ["anytopdf-chunks.json", "anytopdf-manifest.json"]);
    let manifest = json_of(find(&files, "anytopdf-manifest.json"));
    let chunks = json_of(find(&files, "anytopdf-chunks.json"));
    assert_valid("manifest", &manifest);
    assert_valid("chunks", &chunks);
    let source = &manifest["sources"][0];
    let digest = sha256_hex(
        b"Identity fixture
",
    );
    assert_eq!(source["sha256"], digest.as_str());
    assert_eq!(source["size"], 17);
}

#[test]
fn embedded_chunks_trace_every_unit_to_pages() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = setup(tmp.path());
    let png = tmp.path().join("pic.png");
    write_png(&png, 4, 3);
    let pdf = tmp.path().join("out.pdf");
    let dump = tmp.path().join("graph.json");
    convert(&[&notes, &png], &pdf, &[], Some(&dump));

    let files = attachments(&pdf);
    let manifest = json_of(find(&files, "anytopdf-manifest.json"));
    let chunks = json_of(find(&files, "anytopdf-chunks.json"));
    let page_count = page_count(&fs::read(&pdf).unwrap());
    let source_ids: Vec<&Value> = manifest["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| &s["id"])
        .collect();
    let list = chunks["chunks"].as_array().unwrap();
    assert!(!list.is_empty(), "chunks must not be empty");
    for chunk in list {
        let first = chunk["pages"]["first"].as_u64().unwrap();
        let last = chunk["pages"]["last"].as_u64().unwrap();
        assert!(
            first >= 1 && last < page_count,
            "pages {first}..{last} of {page_count}"
        );
        assert!(
            source_ids.contains(&&chunk["source_id"]),
            "unknown source_id"
        );
    }
    let graph: Value = serde_json::from_slice(&fs::read(&dump).unwrap()).unwrap();
    assert_eq!(
        manifest["units"].as_array().unwrap().len(),
        graph["units"].as_array().unwrap().len()
    );
}

#[test]
fn share_profile_manifest_has_no_absolute_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    let notes = setup(&dir);
    let share = dir.join("share.pdf");
    convert(&[&notes], &share, &["--profile", "share"], None);
    let files = attachments(&share);
    let mut all = Vec::new();
    for name in ["anytopdf-manifest.json", "anytopdf-chunks.json"] {
        strings(&json_of(find(&files, name)), &mut all);
    }
    let root = dir.to_string_lossy().to_string();
    assert!(all.iter().all(|s| !s.contains(&root)), "share leaks {root}");

    let archive = dir.join("archive.pdf");
    convert(&[&notes], &archive, &[], None);
    let files = attachments(&archive);
    let manifest = json_of(find(&files, "anytopdf-manifest.json"));
    let source = &manifest["sources"][0];
    let has_path = source["path"].as_str().is_some_and(|p| p.contains(&root))
        || source["metadata"]["source.path"]
            .as_str()
            .is_some_and(|p| p.contains(&root));
    assert!(has_path, "archive profile should record the source path");
}

#[test]
fn convert_writes_no_sidecar_when_embedding_succeeds() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = setup(tmp.path());
    let pdf = tmp.path().join("out.pdf");
    let out = convert(&[&notes], &pdf, &[], None);
    let files = attachments(&pdf);
    assert_eq!(files.len(), 2, "precondition: both attachments embedded");
    assert!(!tmp.path().join("out.pdf.manifest.json").exists());
    assert!(!tmp.path().join("out.pdf.chunks.json").exists());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("[manifest.sidecar]"));
}
