use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn ocr_help_lists_every_choice() {
    let out = run(&["convert", "--help"]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let clause = text
        .split("--ocr-mode")
        .nth(1)
        .unwrap_or_else(|| panic!("no --ocr-mode option in:\n{text}"))
        .split("possible values")
        .nth(1)
        .unwrap_or_else(|| panic!("no `possible values` clause in:\n{text}"));
    let clause = clause.lines().next().unwrap_or_default();
    assert!(
        text.contains("[alias: --ocr]"),
        "--ocr must stay an alias:\n{text}"
    );
    for choice in ["auto", "vision", "doctr", "tesseract", "off"] {
        assert!(clause.contains(choice), "missing {choice} in `{clause}`");
    }
}

#[test]
fn every_subcommand_help_prints_a_description() {
    let mut bad = Vec::new();
    for sub in [
        "convert",
        "probe",
        "doctor",
        "plugins",
        "capabilities",
        "extract",
        "watch",
    ] {
        let out = run(&[sub, "--help"]);
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        if !out.status.success()
            || first.trim().is_empty()
            || first.trim_start().starts_with("Usage:")
        {
            bad.push(format!("{sub}: first line `{first}`"));
        }
    }
    assert!(bad.is_empty(), "no description: {}", bad.join("; "));
}

#[test]
fn ocr_none_alias_still_disables_ocr() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("notes.txt");
    std::fs::write(&input, "hello").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .current_dir(dir.path())
        .args(["convert", "notes.txt", "--ocr", "none"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
