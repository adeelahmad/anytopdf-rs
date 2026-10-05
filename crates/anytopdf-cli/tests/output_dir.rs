use anytopdf_pdf::read_embedded_files;

#[path = "common/output.rs"]
mod output;
use output::stderr;
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

fn run(cwd: &Path, args: &[&Path], extra: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", "")
        .env_remove("SOURCE_DATE_EPOCH")
        .current_dir(cwd)
        .args(args)
        .args(["--ocr", "off"])
        .args(extra)
        .output()
        .unwrap()
}

fn manifest(pdf: &Path) -> Value {
    let files = read_embedded_files(&fs::read(pdf).unwrap()).unwrap();
    let file = files
        .iter()
        .find(|f| f.name == "anytopdf-manifest.json")
        .unwrap_or_else(|| panic!("manifest missing in {}", pdf.display()));
    serde_json::from_slice(&file.bytes).unwrap()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|rd| {
            rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn root() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    (tmp, dir)
}

#[test]
fn output_dir_writes_one_pdf_per_input_with_its_own_manifest() {
    let (_guard, dir) = root();
    let a = dir.join("a.txt");
    let b = dir.join("b.png");
    fs::write(&a, "alpha text\n").unwrap();
    write_png(&b, 4, 3);
    let out_dir = dir.join("out");
    let out = run(&dir, &[&a, &b], &["--output-dir", "out"]);
    assert!(out.status.success(), "{}", stderr(&out));
    for (stem, input) in [("a", "a.txt"), ("b", "b.png")] {
        let pdf = out_dir.join(format!("{stem}.pdf"));
        assert!(pdf.exists(), "{stem}.pdf missing; stderr: {}", stderr(&out));
        assert!(fs::read(&pdf).unwrap().starts_with(b"%PDF-"));
        let m = manifest(&pdf);
        let sources = m["sources"].as_array().unwrap();
        assert_eq!(sources.len(), 1, "{stem}.pdf must list exactly one source");
        assert!(
            sources[0]["name"].as_str().unwrap().contains(input),
            "source name {} should match {input}",
            sources[0]["name"]
        );
    }
    assert!(!out_dir.join("anytopdf.pdf").exists());
}

#[test]
fn output_dir_names_are_deterministic_and_collision_free() {
    let (_guard, dir) = root();
    let (x, y) = (dir.join("x/notes.txt"), dir.join("y/notes.txt"));
    fs::create_dir_all(x.parent().unwrap()).unwrap();
    fs::create_dir_all(y.parent().unwrap()).unwrap();
    fs::write(&x, "first").unwrap();
    fs::write(&y, "second").unwrap();
    let reference = dir.join("ref.pdf");
    let r = run(&dir, &[&x], &["-o", "ref.pdf"]);
    assert!(r.status.success(), "{}", stderr(&r));
    let first_digest = manifest(&reference)["sources"][0]["sha256"].clone();
    assert!(first_digest.is_string());
    for name in ["o1", "o2"] {
        let out = run(&dir, &[&x, &y], &["--output-dir", name]);
        assert!(out.status.success(), "{}", stderr(&out));
        let target = dir.join(name);
        assert_eq!(listing(&target), ["notes-1.pdf", "notes.pdf"], "{name}");
        assert_eq!(
            manifest(&target.join("notes.pdf"))["sources"][0]["sha256"],
            first_digest,
            "{name}: notes.pdf must come from the first input"
        );
    }
}

#[test]
fn existing_file_is_kept_and_a_numbered_name_is_used() {
    let (_guard, dir) = root();
    let a = dir.join("a.txt");
    fs::write(&a, "alpha\n").unwrap();
    fs::create_dir(dir.join("out")).unwrap();
    fs::write(dir.join("out/a.pdf"), "keep").unwrap();
    let out = run(&dir, &[&a], &["--output-dir", "out"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(fs::read(dir.join("out/a.pdf")).unwrap(), b"keep");
    let numbered = dir.join("out/a-1.pdf");
    assert!(
        numbered.exists(),
        "a-1.pdf missing; stderr: {}",
        stderr(&out)
    );
    assert!(fs::read(&numbered).unwrap().starts_with(b"%PDF-"));
    assert!(stderr(&out).contains("a-1.pdf"), "{}", stderr(&out));
}

#[test]
fn output_dir_never_writes_over_a_source() {
    let (_guard, dir) = root();
    let d = dir.join("d");
    fs::create_dir(&d).unwrap();
    let (txt, pdf) = (d.join("a.txt"), d.join("a.pdf"));
    fs::write(&txt, "converted text\n").unwrap();
    fs::write(&pdf, "source").unwrap();
    let out = run(&dir, &[&txt, &pdf], &["--output-dir", "d", "--overwrite"]);
    assert_eq!(fs::read(&pdf).unwrap(), b"source", "{}", stderr(&out));
    let numbered = d.join("a-1.pdf");
    assert!(
        numbered.exists(),
        "a-1.pdf missing; stderr: {}",
        stderr(&out)
    );
    assert!(fs::read(&numbered).unwrap().starts_with(b"%PDF-"));
}

#[test]
fn output_dir_conflicts_with_output_flag() {
    let (_guard, dir) = root();
    let a = dir.join("a.txt");
    fs::write(&a, "alpha\n").unwrap();
    let out = run(&dir, &[&a], &["--output-dir", "out", "-o", "x.pdf"]);
    assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
    assert!(!dir.join("x.pdf").exists());
    assert!(!dir.join("out").exists() || listing(&dir.join("out")).is_empty());
}
