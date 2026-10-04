use anytopdf_core::schema;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend(data);
    out.extend(&body);
    out.extend(crc32(&body).to_be_bytes());
}

/// A width x height 8-bit grayscale PNG using stored deflate blocks.
fn write_png(path: &Path, width: u32, height: u32) {
    let mut raw = Vec::new();
    for _ in 0..height {
        raw.push(0);
        raw.extend(std::iter::repeat_n(128u8, width as usize));
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend((raw.len() as u16).to_le_bytes());
    z.extend((!(raw.len() as u16)).to_le_bytes());
    z.extend(&raw);
    z.extend(((b << 16) | a).to_be_bytes());
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 0, 0, 0, 0]);
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &z);
    png_chunk(&mut png, b"IEND", &[]);
    fs::write(path, png).unwrap();
}

fn events_schema() -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root
        .ancestors()
        .map(|dir| dir.join("schemas").join("events.schema.json"))
        .find(|candidate| candidate.is_file())
        .expect("schemas/events.schema.json must exist");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn base(dir: &Path, inputs: &[&str], extra: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    cmd.current_dir(dir)
        .arg("--no-plugins")
        .env_remove("SOURCE_DATE_EPOCH")
        .arg("convert")
        .args(inputs)
        .args(extra);
    cmd
}

fn mixed(dir: &Path) {
    fs::write(dir.join("a.txt"), "alpha notes\n").unwrap();
    fs::write(dir.join("blob.xyz"), [0u8; 16]).unwrap();
}

fn stream(out: &Output) -> Vec<Value> {
    let schema = events_schema();
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!text.is_empty(), "stderr must carry the event stream");
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            assert!(!line.starts_with("error:"), "human line leaked: {line}");
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stderr line {i} is not JSON ({e}): {line}"));
            let errors = schema::validate(&schema, &value);
            assert!(errors.is_empty(), "line {i} invalid: {line}");
            value
        })
        .collect()
}

fn assert_failed_terminal(events: &[Value], code: i64) {
    let finished: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e["event"] == "run.finished")
        .map(|(i, _)| i)
        .collect();
    assert_eq!(finished.len(), 1, "exactly one run.finished");
    assert_eq!(finished[0], events.len() - 1, "run.finished must be last");
    let last = &events[events.len() - 1];
    assert_eq!(last["status"], "failed");
    assert_eq!(last["exit_code"], code);
    assert!(
        last["error"].as_str().is_some_and(|s| !s.is_empty()),
        "error must be non-empty: {last}"
    );
}

fn strict_like(flag: &str, code: i64) {
    let tmp = tempfile::tempdir().unwrap();
    mixed(tmp.path());
    let out = base(
        tmp.path(),
        &["a.txt", "blob.xyz"],
        &[flag, "--events", "--ocr", "off", "-o", "out.pdf"],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(code as i32));
    assert_failed_terminal(&stream(&out), code);
    assert!(!tmp.path().join("out.pdf").exists());
}

#[test]
fn strict_failure_ends_with_one_failed_run_finished() {
    strict_like("--strict", 5);
}

#[test]
fn fail_fast_failure_ends_with_one_failed_run_finished() {
    strict_like("--fail-fast", 7);
}

#[test]
fn existing_output_without_overwrite_ends_with_one_failed_run_finished() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.txt"), "alpha notes\n").unwrap();
    fs::write(tmp.path().join("out.pdf"), "keep").unwrap();
    let out = base(tmp.path(), &["a.txt"], &["-o", "out.pdf", "--events"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let events = stream(&out);
    assert_eq!(events[0]["event"], "run.started");
    assert_failed_terminal(&events, 2);
    assert_eq!(
        fs::read_to_string(tmp.path().join("out.pdf")).unwrap(),
        "keep"
    );
}

#[test]
fn explicit_tesseract_without_tesseract_ends_with_one_failed_run_finished() {
    let tmp = tempfile::tempdir().unwrap();
    write_png(&tmp.path().join("b.png"), 8, 8);
    let out = base(
        tmp.path(),
        &["b.png"],
        &["--ocr", "tesseract", "--events", "-o", "out.pdf"],
    )
    .env("PATH", "")
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(4));
    let events = stream(&out);
    let diag = events
        .iter()
        .position(|e| e["event"] == "diagnostic" && e["code"] == "enrichment.failed")
        .expect("enrichment.failed diagnostic");
    assert!(diag < events.len() - 1);
    assert_failed_terminal(&events, 4);
}

fn dropped_pipe(extra: &[&str]) -> (Option<i32>, bool) {
    let tmp = tempfile::tempdir().unwrap();
    mixed(tmp.path());
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let mut cmd = base(tmp.path(), &["a.txt", "blob.xyz"], extra);
    let status = cmd
        .arg("-o")
        .arg("out.pdf")
        .stdout(Stdio::null())
        .stderr(writer)
        .status()
        .unwrap();
    (status.code(), tmp.path().join("out.pdf").exists())
}

#[test]
fn dropped_stderr_pipe_on_a_failing_run_never_panics() {
    let (code, exists) = dropped_pipe(&["--strict", "--events"]);
    assert_eq!(code, Some(5));
    assert!(!exists);
}

#[test]
fn dropped_stderr_pipe_on_a_successful_run_never_panics() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.txt"), "alpha notes\n").unwrap();
    let (reader, writer) = std::io::pipe().unwrap();
    drop(reader);
    let status = base(tmp.path(), &["a.txt"], &["--events", "-o", "out.pdf"])
        .stdout(Stdio::null())
        .stderr(writer)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert!(tmp.path().join("out.pdf").exists());
}
