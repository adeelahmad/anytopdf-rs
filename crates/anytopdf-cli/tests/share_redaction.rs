use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Fixture {
    _guard: tempfile::TempDir,
    dir: PathBuf,
    needles: Vec<String>,
}

fn short_form(dir: &Path) -> Option<String> {
    if !cfg!(windows) {
        return None;
    }
    let out = Command::new("cmd")
        .args(["/C", "for", "%I", "in"])
        .arg(format!("(\"{}\")", dir.display()))
        .args(["do", "@echo", "%~sI"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn fixture() -> Fixture {
    let guard = tempfile::tempdir().unwrap();
    let dir = guard.path().to_path_buf();
    let canonical = fs::canonicalize(&dir).unwrap();
    let mut needles = vec![dir.to_string_lossy().into_owned()];
    let canon = canonical.to_string_lossy().into_owned();
    needles.push(canon.clone());
    if let Some(stripped) = canon.strip_prefix(r"\\?\") {
        needles.push(stripped.to_string());
    }
    needles.extend(short_form(&dir));
    needles.extend(short_form(&canonical));
    needles.sort();
    needles.dedup();
    Fixture {
        _guard: guard,
        dir,
        needles,
    }
}

fn write_truncated_png(path: &Path) {
    let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    bytes.extend([0, 0, 0, 13]);
    fs::write(path, bytes).unwrap();
}

fn convert(inputs: &[&Path], pdf: &Path, extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", "")
        .env_remove("SOURCE_DATE_EPOCH")
        .arg("convert")
        .args(inputs)
        .args(["--ocr", "off", "--profile", "share", "-o"])
        .arg(pdf)
        .args(extra)
        .output()
        .unwrap()
}

fn collect(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|v| collect(v, out)),
        serde_json::Value::Object(o) => o.values().for_each(|v| collect(v, out)),
        _ => {}
    }
}

fn json_strings(out: &Output) -> (serde_json::Value, Vec<String>) {
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    let mut strings = Vec::new();
    collect(&value, &mut strings);
    (value, strings)
}

fn assert_stderr_clean(fx: &Fixture, out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    for line in stderr.lines() {
        for needle in &fx.needles {
            assert!(
                !line.contains(needle.as_str()),
                "stderr line leaks {needle:?}: {line}\nfull stderr:\n{stderr}"
            );
        }
    }
}

fn assert_json_clean(fx: &Fixture, strings: &[String]) {
    for s in strings {
        for needle in &fx.needles {
            assert!(
                !s.contains(needle.as_str()),
                "JSON string leaks {needle:?}: {s}"
            );
        }
    }
}

fn mixed_inputs(fx: &Fixture) -> [PathBuf; 3] {
    let a = fx.dir.join("a.txt");
    let bad = fx.dir.join("bad.png");
    let b = fx.dir.join("b.txt");
    fs::write(&a, "alpha\n").unwrap();
    write_truncated_png(&bad);
    fs::write(&b, "beta\n").unwrap();
    [a, bad, b]
}

#[test]
fn share_stderr_warning_and_summary_show_base_names_only() {
    let fx = fixture();
    let [a, bad, b] = mixed_inputs(&fx);
    let pdf = fx.dir.join("out.pdf");
    let out = convert(&[&a, &bad, &b], &pdf, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "exit {:?}\n{stderr}", out.status);
    assert!(
        stderr
            .lines()
            .any(|l| l.contains("WARNING") && l.contains("[import.failed]")),
        "no WARNING [import.failed] line:\n{stderr}"
    );
    assert!(
        stderr.lines().any(|l| l.contains("skipped bad.png:")),
        "no `skipped bad.png:` line:\n{stderr}"
    );
    assert_stderr_clean(&fx, &out);
}

#[test]
fn share_json_diagnostics_and_skipped_messages_have_no_absolute_paths() {
    let fx = fixture();
    let [a, bad, b] = mixed_inputs(&fx);
    let pdf = fx.dir.join("out.pdf");
    let out = convert(&[&a, &bad, &b], &pdf, &["--json"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (value, strings) = json_strings(&out);
    assert_eq!(value["status"], "partial", "{value}");
    assert_eq!(
        value["summary"]["skipped"][0]["input"], "bad.png",
        "{value}"
    );
    let message = value["summary"]["skipped"][0]["message"]
        .as_str()
        .unwrap_or("");
    assert!(!message.is_empty(), "empty skipped message: {value}");
    assert_json_clean(&fx, &strings);
    assert_stderr_clean(&fx, &out);
}

#[test]
fn share_redacts_paths_when_every_input_fails() {
    let fx = fixture();
    let bad1 = fx.dir.join("bad1.png");
    let bad2 = fx.dir.join("bad2.png");
    write_truncated_png(&bad1);
    write_truncated_png(&bad2);
    let pdf = fx.dir.join("out.pdf");
    let out = convert(&[&bad1, &bad2], &pdf, &["--json"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (value, strings) = json_strings(&out);
    assert_eq!(value["status"], "failed", "{value}");
    assert!(!pdf.exists(), "out.pdf must not be written");
    assert_json_clean(&fx, &strings);
    assert_stderr_clean(&fx, &out);
}

#[test]
fn share_missing_absolute_input_is_reported_by_base_name() {
    let fx = fixture();
    let missing = fx.dir.join("missing.txt");
    let pdf = fx.dir.join("out.pdf");
    let out = convert(&[&missing], &pdf, &["--json"]);
    assert!(!out.status.success(), "expected non-zero exit");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let strings = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .map(|v| {
            let mut s = Vec::new();
            collect(&v, &mut s);
            s
        })
        .unwrap_or_default();
    assert!(
        stderr.contains("missing.txt") || strings.iter().any(|s| s.contains("missing.txt")),
        "missing.txt not mentioned.\nstderr: {stderr}\njson: {strings:?}"
    );
    assert_json_clean(&fx, &strings);
    assert_stderr_clean(&fx, &out);
}
