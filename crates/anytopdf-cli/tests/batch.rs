use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[path = "common/output.rs"]
mod output;
use output::stderr;

fn run(dir: &Path, names: &[&str], extra: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    command
        .arg("--no-plugins")
        .env("PATH", "")
        .current_dir(dir)
        .arg("convert");
    for name in names {
        command.arg(dir.join(name));
    }
    command.args(extra).output().unwrap()
}

fn mixed_batch(dir: &Path) {
    fs::write(dir.join("a.txt"), "alpha marker").unwrap();
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend((0u8..32).map(|byte| byte.wrapping_mul(37).wrapping_add(11)));
    fs::write(dir.join("corrupt.png"), png).unwrap();
    fs::write(dir.join("b.txt"), "beta marker").unwrap();
}

fn out(dir: &Path) -> PathBuf {
    dir.join("out.pdf")
}

#[test]
fn corrupt_file_mid_batch_is_skipped_and_run_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    mixed_batch(dir.path());
    let graph = dir.path().join("g.json");
    let output = out(dir.path());
    let result = run(
        dir.path(),
        &["a.txt", "corrupt.png", "b.txt"],
        &[
            "--ocr",
            "off",
            "-o",
            output.to_str().unwrap(),
            "--dump-graph",
            graph.to_str().unwrap(),
        ],
    );
    let err = stderr(&result);
    assert_eq!(result.status.code(), Some(0), "{err}");
    assert!(fs::read(&output).unwrap().starts_with(b"%PDF-"));
    let dump: serde_json::Value = serde_json::from_slice(&fs::read(&graph).unwrap()).unwrap();
    assert_eq!(dump["sources"].as_array().unwrap().len(), 2, "{err}");
    let text: String = dump["units"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|unit| unit["visible_text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        text.contains("alpha marker") && text.contains("beta marker"),
        "{text}"
    );
    assert!(
        err.lines()
            .any(|line| line.contains("WARNING [") && line.contains("corrupt.png")),
        "{err}"
    );
    assert!(err.contains("Summary: 2 converted, 1 skipped"), "{err}");
}

#[test]
fn unsupported_file_mid_batch_is_skipped_and_run_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    fs::write(dir.path().join("valid.txt"), "valid notes").unwrap();
    fs::write(dir.path().join("invalid.bin"), [0u8; 16]).unwrap();
    let output = out(dir.path());
    let result = run(
        dir.path(),
        &["valid.txt", "invalid.bin"],
        &["-o", output.to_str().unwrap()],
    );
    let err = stderr(&result);
    assert_eq!(result.status.code(), Some(0), "{err}");
    assert!(output.exists());
    assert!(
        err.lines().any(|line| line.contains("skipped")
            && line.contains("invalid.bin")
            && line.contains("[input.unsupported]")),
        "{err}"
    );
}

#[test]
fn fail_fast_aborts_without_publishing_and_exits_7() {
    let dir = tempfile::tempdir().unwrap();
    mixed_batch(dir.path());
    let output = out(dir.path());
    let result = run(
        dir.path(),
        &["a.txt", "corrupt.png", "b.txt"],
        &[
            "--ocr",
            "off",
            "--fail-fast",
            "-o",
            output.to_str().unwrap(),
        ],
    );
    let err = stderr(&result);
    assert_eq!(result.status.code(), Some(7), "{err}");
    assert!(!output.exists());
    assert!(err.contains("corrupt.png"), "{err}");
}

#[test]
fn strict_still_rejects_a_batch_with_skips() {
    let dir = tempfile::tempdir().unwrap();
    mixed_batch(dir.path());
    let output = out(dir.path());
    let result = run(
        dir.path(),
        &["a.txt", "corrupt.png", "b.txt"],
        &["--ocr", "off", "--strict", "-o", output.to_str().unwrap()],
    );
    let err = stderr(&result);
    assert_eq!(result.status.code(), Some(5), "{err}");
    assert!(!output.exists());
}

#[test]
fn batch_where_every_file_fails_is_an_input_error() {
    let dir = tempfile::tempdir().unwrap();
    mixed_batch(dir.path());
    fs::write(dir.path().join("invalid.bin"), [0u8; 16]).unwrap();
    let output = out(dir.path());
    let result = run(
        dir.path(),
        &["corrupt.png", "invalid.bin"],
        &["--ocr", "off", "-o", output.to_str().unwrap()],
    );
    let err = stderr(&result);
    assert_eq!(result.status.code(), Some(3), "{err}");
    assert!(!output.exists());
    assert!(
        err.contains("corrupt.png") && err.contains("invalid.bin"),
        "{err}"
    );
}
