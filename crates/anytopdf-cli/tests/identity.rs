use std::{fs, path::Path, process::Command};

const DIGEST: &str = "eea2ca13a1da285c9365c7dd3fdfb68eb34445313f8eb1b994991728ae3d917a";

fn convert(input: &Path, pdf: &Path, json: &Path) -> serde_json::Value {
    let result = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", "")
        .arg("convert")
        .arg(input)
        .args(["--ocr", "off", "-o"])
        .arg(pdf)
        .arg("--dump-graph")
        .arg(json)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&fs::read(json).unwrap()).unwrap()
}

#[test]
fn repeated_conversion_dumps_identical_ids_and_digest() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("notes.txt");
    fs::write(&input, b"Identity fixture\n").unwrap();
    let a = convert(
        &input,
        &dir.path().join("a.pdf"),
        &dir.path().join("a.json"),
    );
    let b = convert(
        &input,
        &dir.path().join("b.pdf"),
        &dir.path().join("b.json"),
    );
    assert_eq!(a["sources"][0]["id"], b["sources"][0]["id"]);
    let (ua, ub) = (
        a["units"].as_array().unwrap(),
        b["units"].as_array().unwrap(),
    );
    assert_eq!(ua.len(), ub.len());
    for (x, y) in ua.iter().zip(ub) {
        assert_eq!(x["id"], y["id"]);
    }
    assert_eq!(a["sources"][0]["sha256"], DIGEST);
    assert_eq!(a["sources"][0]["size"], 17);
}

#[path = "common/png.rs"]
mod png;
#[path = "common/png_gray.rs"]
mod png_gray;
use png_gray::write_png;

#[test]
fn graph_dump_units_carry_kind_appropriate_anchors() {
    let dir = tempfile::tempdir().unwrap();
    let text = dir.path().join("notes.txt");
    fs::write(&text, b"Identity fixture\n").unwrap();
    let image = dir.path().join("image.png");
    write_png(&image, 4, 3);
    let dump = convert_all(&[&text, &image], dir.path());
    let units = dump["units"].as_array().unwrap();
    assert_eq!(units.len(), 2);
    for unit in units {
        assert!(unit.get("anchor").is_some(), "unit without anchor: {unit}");
    }
    let by_kind = |kind: &str| units.iter().find(|u| u["kind"] == kind).unwrap();
    let anchor = &by_kind("text")["anchor"];
    assert_eq!(anchor["kind"], "byte-range");
    assert_eq!(anchor["start"], 0);
    assert_eq!(anchor["end"], 17);
    let anchor = &by_kind("visual")["anchor"];
    assert_eq!(anchor["kind"], "region");
    assert_eq!(anchor["width"], 1.0);
    assert_eq!(anchor["height"], 1.0);
}

fn convert_all(inputs: &[&Path], dir: &Path) -> serde_json::Value {
    let json = dir.join("all.json");
    let result = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", "")
        .arg("convert")
        .args(inputs)
        .args(["--ocr", "off", "-o"])
        .arg(dir.join("all.pdf"))
        .arg("--dump-graph")
        .arg(&json)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&fs::read(json).unwrap()).unwrap()
}

#[test]
fn gray_png_fixture_bytes_are_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a.png"), dir.path().join("b.png"));
    write_png(&a, 8, 8);
    write_png(&b, 4, 3);
    assert_eq!(
        anytopdf_core::sha256_hex(&fs::read(&a).unwrap()),
        "f78c6580bef3099c7bd64109a30e919326cf8112b31a280e2d4ff667d5883b9b"
    );
    assert_eq!(
        anytopdf_core::sha256_hex(&fs::read(&b).unwrap()),
        "e8c313359eeaf147a590d309f68468e4d504c80fc1613a5d4483ad46300db5d8"
    );
}
