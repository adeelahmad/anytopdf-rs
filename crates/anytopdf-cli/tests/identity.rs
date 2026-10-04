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
