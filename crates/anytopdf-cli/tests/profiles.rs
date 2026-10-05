use anytopdf_core::sha256_hex;
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

fn run(
    inputs: &[&Path],
    pdf: &Path,
    json: Option<&Path>,
    extra: &[&str],
    epoch: Option<&str>,
) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    cmd.arg("--no-plugins")
        .env("PATH", "")
        .env_remove("SOURCE_DATE_EPOCH");
    if let Some(epoch) = epoch {
        cmd.env("SOURCE_DATE_EPOCH", epoch);
    }
    cmd.arg("convert")
        .args(inputs)
        .args(["--ocr", "off", "-o"])
        .arg(pdf);
    if let Some(json) = json {
        cmd.arg("--dump-graph").arg(json);
    }
    cmd.args(extra).output().unwrap()
}

fn ok(out: &Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn load(path: &Path) -> serde_json::Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(s) => out.push(s.clone()),
        serde_json::Value::Array(a) => a.iter().for_each(|v| strings(v, out)),
        serde_json::Value::Object(o) => o.values().for_each(|v| strings(v, out)),
        _ => {}
    }
}

fn all_strings(value: &serde_json::Value) -> Vec<String> {
    let mut out = Vec::new();
    strings(value, &mut out);
    out
}

fn page_count(pdf: &Path) -> u32 {
    let re = regex::bytes::Regex::new(r"/Type\s*/Page[^s]").unwrap();
    re.find_iter(&fs::read(pdf).unwrap()).count() as u32
}

fn pdftotext(pdf: &Path, page: Option<u32>) -> Option<String> {
    let exe = which::which("pdftotext").ok()?;
    let mut cmd = Command::new(exe);
    if let Some(p) = page {
        cmd.args(["-f", &p.to_string(), "-l", &p.to_string()]);
    }
    let out = cmd.arg(pdf).arg("-").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn fixture_dir() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.txt");
    fs::write(&notes, b"Identity fixture\n").unwrap();
    (dir, notes)
}

#[test]
fn same_inputs_and_source_date_epoch_give_identical_pdf_and_dump_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.txt");
    fs::write(&notes, b"Identity fixture\n").unwrap();
    let image = dir.path().join("image.png");
    write_png(&image, 4, 3);
    let epoch = Some("1700000000");
    let paths = |n: &str| {
        (
            dir.path().join(format!("{n}.pdf")),
            dir.path().join(format!("{n}.json")),
        )
    };
    let (apdf, ajson) = paths("a");
    let (bpdf, bjson) = paths("b");
    ok(&run(&[&notes, &image], &apdf, Some(&ajson), &[], epoch));
    ok(&run(&[&notes, &image], &bpdf, Some(&bjson), &[], epoch));
    assert!(
        fs::read(&apdf).unwrap() == fs::read(&bpdf).unwrap(),
        "pdf bytes differ"
    );
    assert!(
        fs::read(&ajson).unwrap() == fs::read(&bjson).unwrap(),
        "dump bytes differ"
    );

    fs::write(&notes, b"Identity fixturf\n").unwrap();
    let (cpdf, cjson) = paths("c");
    ok(&run(&[&notes, &image], &cpdf, Some(&cjson), &[], epoch));
    let (a, c) = (load(&ajson), load(&cjson));
    assert_ne!(a["sources"][0]["id"], c["sources"][0]["id"]);
    assert_ne!(a["sources"][0]["sha256"], c["sources"][0]["sha256"]);
    assert_eq!(a["sources"][1]["id"], c["sources"][1]["id"]);
}

#[test]
fn share_profile_keeps_absolute_paths_out_of_graph_dump() {
    let (dir, notes) = fixture_dir();
    let raw = dir.path().to_string_lossy().into_owned();
    let root = fs::canonicalize(dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let bare = root.strip_prefix(r"\\?\").unwrap_or(&root).to_owned();
    let (pdf, json) = (dir.path().join("s.pdf"), dir.path().join("s.json"));
    ok(&run(
        &[&notes],
        &pdf,
        Some(&json),
        &["--profile", "share"],
        None,
    ));
    let dump = load(&json);
    let leaks: Vec<_> = all_strings(&dump)
        .into_iter()
        .filter(|s| s.contains(&root) || s.contains(&bare) || s.contains(&raw))
        .collect();
    assert!(leaks.is_empty(), "share dump leaks: {leaks:?}");
    assert_eq!(dump["sources"][0]["path"], "notes.txt");

    let (pdf, json) = (dir.path().join("d.pdf"), dir.path().join("d.json"));
    ok(&run(&[&notes], &pdf, Some(&json), &[], None));
    let dump = load(&json);
    let path = dump["sources"][0]["metadata"]["source.path"]
        .as_str()
        .unwrap_or("");
    assert!(
        path.contains(&root) || path.contains(&bare),
        "archive dump lacks source.path under {root}: {path:?}"
    );
}

#[test]
fn graph_dump_never_references_the_deleted_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("image.png");
    write_png(&image, 4, 3);
    let (pdf, json) = (dir.path().join("o.pdf"), dir.path().join("o.json"));
    ok(&run(&[&image], &pdf, Some(&json), &[], None));
    let dump = load(&json);
    for unit in dump["units"].as_array().unwrap() {
        assert!(unit["visual_path"].is_null(), "visual_path kept: {unit}");
    }
    let root = &std::fs::canonicalize(dir.path()).unwrap();
    for s in all_strings(&dump) {
        assert!(!s.contains("anytopdf-"), "workspace prefix leaked: {s}");
        let p = Path::new(&s);
        if p.is_absolute() {
            assert!(
                p.starts_with(root) || !p.exists(),
                "dangling absolute path: {s}"
            );
        }
    }
}

#[test]
fn provenance_page_text_matches_independent_sha256() {
    let (dir, notes) = fixture_dir();
    let digest = sha256_hex(b"Identity fixture\n");
    let root = dir.path().to_string_lossy().into_owned();
    for profile in ["archive", "share"] {
        let pdf = dir.path().join(format!("{profile}.pdf"));
        ok(&run(&[&notes], &pdf, None, &["--profile", profile], None));
        let pages = page_count(&pdf);
        let (Some(first), Some(last)) = (pdftotext(&pdf, Some(1)), pdftotext(&pdf, Some(pages)))
        else {
            println!("POPPLER-UNAVAILABLE: skipping text assertions for profile {profile}");
            continue;
        };
        assert!(first.contains("Identity fixture"), "page 1: {first}");
        assert!(
            last.contains(&format!("SHA-256: {digest}")),
            "last page: {last}"
        );
        assert!(
            last.contains(&format!("Profile: {profile}")),
            "last page: {last}"
        );
        assert!(last.contains("Source: notes.txt"), "last page: {last}");
        assert!(!last.contains(&root), "last page leaks temp dir: {last}");
    }
}

#[test]
fn no_provenance_page_flag_omits_the_page_but_keeps_page_count_of_content() {
    let (dir, notes) = fixture_dir();
    let (with, without) = (dir.path().join("with.pdf"), dir.path().join("without.pdf"));
    ok(&run(&[&notes], &with, None, &[], None));
    ok(&run(
        &[&notes],
        &without,
        None,
        &["--no-provenance-page"],
        None,
    ));
    assert_eq!(page_count(&with), page_count(&without) + 1);
    match pdftotext(&without, None) {
        Some(text) => assert!(
            !text.contains("SHA-256:"),
            "provenance text present: {text}"
        ),
        None => println!("POPPLER-UNAVAILABLE: skipping text assertion"),
    }
}

#[test]
fn invalid_source_date_epoch_is_a_usage_error() {
    let (dir, notes) = fixture_dir();
    let pdf = dir.path().join("bad.pdf");
    let out = run(&[&notes], &pdf, None, &[], Some("yesterday"));
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!pdf.exists(), "output written despite usage error");
}
