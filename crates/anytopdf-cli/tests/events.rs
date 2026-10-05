use anytopdf_core::schema;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

#[path = "common/png.rs"]
mod png;
#[path = "common/png_gray.rs"]
mod png_gray;
use png_gray::write_png;

fn page_count(pdf: &[u8]) -> u64 {
    let text = String::from_utf8_lossy(pdf);
    regex::Regex::new(r"/Type\s*/Page\b")
        .unwrap()
        .find_iter(&text)
        .count() as u64
}

fn events_schema() -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root
        .ancestors()
        .map(|dir| dir.join("schemas").join("events.schema.json"))
        .find(|candidate| candidate.is_file());
    let path = path.expect("schemas/events.schema.json must exist");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn mixed(dir: &Path) {
    fs::write(dir.join("a.txt"), "alpha notes\n").unwrap();
    write_png(&dir.join("b.png"), 8, 8);
    fs::write(dir.join("blob.xyz"), [0u8; 16]).unwrap();
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

fn stream(out: &Output) -> Vec<Value> {
    let schema = events_schema();
    let text = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(!text.is_empty(), "stderr must carry the event stream");
    text.lines()
        .enumerate()
        .map(|(i, line)| {
            let value: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("stderr line {i} is not JSON ({e}): {line}"));
            let errors = schema::validate(&schema, &value);
            assert!(errors.is_empty(), "line {i} invalid: {line}");
            assert_eq!(value["seq"], i as u64, "seq must equal the line index");
            value
        })
        .collect()
}

fn names(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|e| e["event"].as_str().unwrap_or("").to_string())
        .collect()
}

fn collect(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| collect(v, out)),
        Value::Object(o) => o.values().for_each(|v| collect(v, out)),
        _ => {}
    }
}

#[test]
fn mixed_run_emits_schema_valid_gapless_lifecycle_stream() {
    let tmp = tempfile::tempdir().unwrap();
    mixed(tmp.path());
    let inputs = ["a.txt", "b.png", "blob.xyz"];
    let plain = base(tmp.path(), &inputs, &["-o", "plain.pdf", "--ocr", "auto"])
        .output()
        .unwrap();
    let out = base(
        tmp.path(),
        &inputs,
        &["-o", "out.pdf", "--events", "--ocr", "auto"],
    )
    .output()
    .unwrap();
    assert_eq!(out.status.code(), plain.status.code());
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = stream(&out);
    assert_eq!(events[0]["event"], "run.started");
    assert_eq!(events[0]["inputs"], 3);
    let stages: Vec<(String, String)> = events
        .iter()
        .filter(|e| e["event"] == "stage.started" || e["event"] == "stage.finished")
        .map(|e| {
            (
                e["event"].as_str().unwrap().to_string(),
                e["stage"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for stage in ["discover", "import", "enrich", "render"] {
        let s = stages
            .iter()
            .position(|x| x == &("stage.started".into(), stage.into()));
        let f = stages
            .iter()
            .position(|x| x == &("stage.finished".into(), stage.into()));
        assert!(
            matches!((s, f), (Some(s), Some(f)) if s < f),
            "stage {stage}: {stages:?}"
        );
    }
    let order: Vec<&str> = ["discover", "import", "enrich", "render"].to_vec();
    let started: Vec<&str> = stages
        .iter()
        .filter(|x| x.0 == "stage.started")
        .map(|x| x.1.as_str())
        .collect();
    assert_eq!(started, order);
    for (index, input) in [(0, "a.txt"), (1, "b.png")] {
        assert!(
            events
                .iter()
                .any(|e| e["event"] == "source.started" && e["index"] == index)
        );
        assert!(
            events.iter().any(|e| e["event"] == "source.imported"
                && e["index"] == index
                && e["input"].as_str().is_some_and(|p| p.ends_with(input))),
            "{input} not imported"
        );
    }
    assert!(events.iter().any(|e| e["event"] == "source.skipped"
        && e["code"] == "input.unsupported"
        && e["input"].as_str().is_some_and(|p| p.ends_with("blob.xyz"))));
    let n = names(&events);
    assert!(n.contains(&"unit.started".to_string()) && n.contains(&"unit.finished".to_string()));
    assert!(n.contains(&"diagnostic".to_string()));
    let pdf = events
        .iter()
        .find(|e| e["event"] == "output.written" && e["kind"] == "pdf")
        .expect("pdf output.written");
    let pages = page_count(&fs::read(tmp.path().join("out.pdf")).unwrap());
    assert_eq!(pdf["pages"], pages);
    let last = events.last().unwrap();
    assert_eq!(last["event"], "run.finished");
    assert_eq!(last["status"], "partial");
    assert_eq!(last["exit_code"], 0);
    for line in String::from_utf8_lossy(&out.stderr).lines() {
        for bad in ["WARNING", "INFO", "Summary:", "Wrote", "error:"] {
            assert!(!line.starts_with(bad), "human line leaked: {line}");
        }
    }
}

#[test]
fn events_leave_stdout_pdf_and_exit_code_unchanged() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.txt"), "alpha\n").unwrap();
    write_png(&tmp.path().join("b.png"), 8, 8);
    let args = ["-o", "out.pdf", "--json", "--ocr", "off", "--overwrite"];
    let run = |extra: &[&str]| {
        let out = base(tmp.path(), &["a.txt", "b.png"], &args)
            .args(extra)
            .env("SOURCE_DATE_EPOCH", "1700000000")
            .env("PATH", "")
            .output()
            .unwrap();
        (
            out,
            fs::read(tmp.path().join("out.pdf")).unwrap_or_default(),
        )
    };
    let (plain, plain_pdf) = run(&[]);
    let (with, with_pdf) = run(&["--events"]);
    assert_eq!(with.status.code(), plain.status.code());
    assert_eq!(with.status.code(), Some(0), "--events must be accepted");
    assert_eq!(with.stdout, plain.stdout);
    assert_eq!(with_pdf, plain_pdf);
    let mut files: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(files, ["a.txt", "b.png", "out.pdf"]);
}

#[test]
fn identical_runs_emit_byte_identical_event_streams() {
    let tmp = tempfile::tempdir().unwrap();
    mixed(tmp.path());
    let run = || {
        base(
            tmp.path(),
            &["a.txt", "b.png", "blob.xyz"],
            &["-o", "out.pdf", "--events", "--ocr", "off", "--overwrite"],
        )
        .env("SOURCE_DATE_EPOCH", "1700000000")
        .output()
        .unwrap()
    };
    let (first, second) = (run(), run());
    assert!(!first.stderr.is_empty(), "event stream must not be empty");
    assert_eq!(first.stderr, second.stderr);
    for line in String::from_utf8_lossy(&first.stderr).lines() {
        serde_json::from_str::<Value>(line).expect("every line parses");
    }
}

#[test]
fn share_profile_events_contain_no_absolute_or_workspace_paths() {
    let guard = tempfile::tempdir().unwrap();
    let dir = guard.path().to_path_buf();
    mixed(&dir);
    let canonical = fs::canonicalize(&dir).unwrap();
    let canon = canonical.to_string_lossy().into_owned();
    let mut needles = vec![dir.to_string_lossy().into_owned(), canon.clone()];
    if let Some(s) = canon.strip_prefix(r"\\?\") {
        needles.push(s.to_string());
    }
    needles.push("anytopdf-".to_string());
    let out = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", "")
        .env_remove("SOURCE_DATE_EPOCH")
        .arg("convert")
        .args(["a.txt", "b.png", "blob.xyz"].map(|n| dir.join(n)))
        .args(["--profile", "share", "--events", "--dump-graph"])
        .arg(dir.join("g.json"))
        .arg("-o")
        .arg(dir.join("out.pdf"))
        .args(["--ocr", "off"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let events = stream(&out);
    let mut strings = Vec::new();
    events.iter().for_each(|e| collect(e, &mut strings));
    for s in &strings {
        for needle in &needles {
            assert!(!s.contains(needle.as_str()), "{s:?} leaks {needle:?}");
        }
    }
    let written = |kind: &str| {
        events
            .iter()
            .find(|e| e["event"] == "output.written" && e["kind"] == kind)
            .and_then(|e| e["path"].as_str().map(str::to_string))
    };
    assert_eq!(written("pdf").as_deref(), Some("out.pdf"));
    assert_eq!(written("graph").as_deref(), Some("g.json"));
    let skipped = events
        .iter()
        .find(|e| e["event"] == "source.skipped")
        .unwrap();
    assert_eq!(skipped["input"], "blob.xyz");
}

#[test]
fn archive_event_paths_match_json_output() {
    let tmp = tempfile::tempdir().unwrap();
    mixed(tmp.path());
    let out = base(
        tmp.path(),
        &["a.txt", "b.png", "blob.xyz"],
        &["--json", "--events", "--ocr", "off", "-o", "out.pdf"],
    )
    .env("PATH", "")
    .output()
    .unwrap();
    let events = stream(&out);
    let doc: Value = serde_json::from_slice(&out.stdout).unwrap();
    let pdf = events
        .iter()
        .find(|e| e["event"] == "output.written" && e["kind"] == "pdf")
        .expect("pdf output.written");
    assert_eq!(pdf["path"], doc["outputs"][0]["path"]);
    assert_eq!(pdf["pages"], doc["outputs"][0]["pages"]);
    let skipped = events
        .iter()
        .find(|e| e["event"] == "source.skipped")
        .unwrap();
    assert_eq!(skipped["input"], doc["summary"]["skipped"][0]["input"]);
    let last = events.last().unwrap();
    assert_eq!(last["event"], "run.finished");
    assert_eq!(last["status"], doc["status"]);
    assert_eq!(last["exit_code"], doc["exit_code"]);
}

#[test]
fn quiet_with_events_still_emits_the_stream() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("a.txt"), "alpha\n").unwrap();
    let out = base(
        tmp.path(),
        &["a.txt"],
        &["--quiet", "--events", "--ocr", "off", "-o", "out.pdf"],
    )
    .env("PATH", "")
    .output()
    .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let events = stream(&out);
    assert_eq!(events[0]["event"], "run.started");
    let last = events.last().unwrap();
    assert_eq!(last["event"], "run.finished");
    assert_eq!(last["status"], "ok");
}
