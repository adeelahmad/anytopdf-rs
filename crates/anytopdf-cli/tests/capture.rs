#[path = "common/process.rs"]
mod process;
use process::command;
use serde_json::Value;
use std::{path::Path, process::Command};

/// The binary with the caller's PATH, so FFmpeg can be found.
fn with_ffmpeg() -> Option<Command> {
    if which::which("ffmpeg").is_err() {
        let required = std::env::var_os("ANYTOPDF_REQUIRE_FFMPEG").is_some_and(|v| !v.is_empty());
        assert!(
            !required,
            "ANYTOPDF_REQUIRE_FFMPEG is set but ffmpeg is not on PATH"
        );
        eprintln!("ffmpeg not on PATH; screen recording tests need it");
        return None;
    }
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    command.arg("--no-plugins");
    Some(command)
}

/// A generated test pattern stands in for the screen, so the test needs no display.
const TEST_SOURCE: [&str; 4] = [
    "--input-format",
    "lavfi",
    "--input",
    "life=size=320x240:rate=4:ratio=0.5:seed=1",
];

fn extracted_frames(pdf: &Path) -> Vec<Value> {
    let out = command()
        .arg("extract")
        .arg(pdf)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    doc["manifest"]["units"]
        .as_array()
        .expect("manifest units")
        .iter()
        .filter(|u| u["kind"] == "visual")
        .cloned()
        .collect()
}

#[test]
fn capture_without_ffmpeg_is_a_provider_error_and_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("screen.pdf");
    let out = command()
        .args(["capture", "screen", "--duration", "1", "-o"])
        .arg(&pdf)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&out.stderr).contains("requires ffmpeg"));
    assert!(!pdf.exists());
}

#[test]
fn capture_refuses_a_taken_output_before_recording() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("screen.pdf");
    std::fs::write(&pdf, "keep").unwrap();
    let out = command()
        .args(["capture", "screen", "--duration", "1", "-o"])
        .arg(&pdf)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("output exists"));
    assert_eq!(std::fs::read_to_string(&pdf).unwrap(), "keep");
}

#[test]
fn capture_rejects_an_output_after_the_separator() {
    let out = command()
        .args([
            "capture",
            "screen",
            "--duration",
            "1",
            "--",
            "--output-dir",
            "x",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("capture screen -o"));
}

#[test]
fn recorded_screen_becomes_sampled_pdf_pages() {
    let Some(mut anytopdf) = with_ffmpeg() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("screen.pdf");
    let recording = dir.path().join("screen.mkv");
    let out = anytopdf
        .args(["capture", "screen"])
        .args(TEST_SOURCE)
        .args(["--duration", "3", "--interval", "1", "--keep-recording"])
        .arg(&recording)
        .arg("-o")
        .arg(&pdf)
        .args(["--", "--ocr", "off", "--dedupe-distance", "0"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(recording.metadata().unwrap().len() > 0);
    let frames = extracted_frames(&pdf);
    assert!(
        frames.len() >= 2,
        "expected interval frames, got {frames:#?}"
    );
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_recording_and_still_converts() {
    let Some(mut anytopdf) = with_ffmpeg() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("screen.pdf");
    let child = anytopdf
        .args(["capture", "screen"])
        .args(TEST_SOURCE)
        .arg("-o")
        .arg(&pdf)
        .args(["--", "--ocr", "off", "--quiet"])
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(std::time::Duration::from_secs(3));
    // SAFETY: signals the child we spawned, which is still running.
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let out = child.wait_with_output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!extracted_frames(&pdf).is_empty());
}
