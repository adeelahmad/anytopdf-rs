use std::{fs, process::Command};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    command.arg("--no-plugins").env("PATH", "");
    command
}

#[test]
fn converts_text_and_writes_graph() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.txt");
    let output = dir.path().join("result.pdf");
    let graph = dir.path().join("nested/graph.json");
    fs::write(&source, "Searchable acceptance marker\nA second line.").unwrap();
    let result = command()
        .arg("convert")
        .arg(&source)
        .args(["--ocr", "off", "-o"])
        .arg(&output)
        .arg("--dump-graph")
        .arg(&graph)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(fs::read(output).unwrap().starts_with(b"%PDF-"));
    let graph: serde_json::Value = serde_json::from_slice(&fs::read(graph).unwrap()).unwrap();
    assert!(
        graph["units"][0]["visible_text"]
            .as_str()
            .unwrap()
            .contains("acceptance marker")
    );
}

#[test]
fn conversion_never_overwrites_input() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.txt");
    fs::write(&source, "keep this source").unwrap();
    let result = command()
        .arg("convert")
        .arg(&source)
        .args(["--overwrite", "-o"])
        .arg(&source)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(fs::read_to_string(source).unwrap(), "keep this source");
}

#[test]
fn unsupported_input_fails_without_creating_output() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("unsupported.bin");
    let output = dir.path().join("result.pdf");
    fs::write(&source, [0u8; 16]).unwrap();
    let result = command()
        .arg("convert")
        .arg(source)
        .arg("-o")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
}

#[test]
fn strict_conversion_does_not_publish_partial_success() {
    let dir = tempfile::tempdir().unwrap();
    let valid = dir.path().join("valid.txt");
    let invalid = dir.path().join("invalid.bin");
    let output = dir.path().join("result.pdf");
    fs::write(&valid, "valid text").unwrap();
    fs::write(&invalid, [0u8; 16]).unwrap();
    let result = command()
        .arg("convert")
        .args([valid, invalid])
        .args(["--strict", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!output.exists());
}

#[test]
fn probe_does_not_ingest_or_render_media() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("movie.mp4");
    fs::write(&source, "not actually a video").unwrap();
    let result = command().arg("probe").arg(source).output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let probe: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(probe["importer"]["name"], "ffmpeg-video");
}
