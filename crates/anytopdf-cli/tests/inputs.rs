use std::{fs, process::Command};

#[test]
fn latin1_and_csv_inputs_convert_with_lossy_code() {
    let dir = tempfile::tempdir().unwrap();
    let latin1 = dir.path().join("latin1.txt");
    let csv = dir.path().join("data.csv");
    let output = dir.path().join("out.pdf");
    fs::write(&latin1, b"caf\xe9 r\xe9sum\xe9\n").unwrap();
    fs::write(&csv, "a,b\n1,2\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", "")
        .arg("convert")
        .arg(&latin1)
        .arg(&csv)
        .args(["--ocr", "off", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert_eq!(result.status.code(), Some(0), "{stderr}");
    let pdf = fs::read(&output).unwrap_or_default();
    assert!(pdf.starts_with(b"%PDF-"), "{stderr}");
    assert!(stderr.contains("[input.lossy-decode]"), "{stderr}");
}
