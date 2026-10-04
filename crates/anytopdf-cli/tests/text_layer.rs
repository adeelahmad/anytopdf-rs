use std::{fs, process::Command};

#[test]
fn pdftotext_output_has_content_without_provenance_noise() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("notes.txt");
    let output = dir.path().join("out.pdf");
    let mut body = String::from("Text layer marker\n");
    for i in 0..300 {
        body.push_str(&format!("content line {i}\n"));
    }
    fs::write(&source, body).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", "")
        .arg("convert")
        .arg(&source)
        .args(["--ocr", "off", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let Ok(pdftotext) = which::which("pdftotext") else {
        eprintln!(
            "POPPLER-UNAVAILABLE: pdftotext not found; text_layer CLI assertion not exercised"
        );
        return;
    };
    let extracted = Command::new(pdftotext)
        .args(["-layout"])
        .arg(&output)
        .arg("-")
        .output()
        .unwrap();
    assert!(extracted.status.success());
    let text = String::from_utf8_lossy(&extracted.stdout);
    assert!(text.contains("Text layer marker"));
    for noise in [
        "[SOURCE]",
        "[META]",
        "FileAccessDate",
        "FilePermissions",
        "Directory:",
    ] {
        assert!(!text.contains(noise), "pdftotext output contains {noise}");
    }
    let dir_str = dir.path().display().to_string();
    assert!(!text.contains(&dir_str), "pdftotext output has temp path");
}
