use std::fs;
use std::path::{Path, PathBuf};

const MAX_LINES: usize = 800;

fn src_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read(name: &str) -> String {
    fs::read_to_string(src_dir().join(name)).unwrap_or_default()
}

#[test]
fn pdf_source_files_stay_under_800_lines() {
    let mut files = Vec::new();
    for entry in fs::read_dir(src_dir()).expect("src dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_some_and(|e| e == "rs") {
            let lines = fs::read_to_string(&path).expect("read").lines().count();
            files.push((
                path.file_name().unwrap().to_string_lossy().into_owned(),
                lines,
            ));
        }
    }
    let offenders: Vec<String> = files
        .iter()
        .filter(|(_, n)| *n >= MAX_LINES)
        .map(|(name, n)| format!("{name} has {n} lines"))
        .collect();
    assert!(
        offenders.is_empty() && files.len() >= 5,
        "need >= 5 files (found {}) and none >= {MAX_LINES} lines; offenders: {offenders:?}",
        files.len()
    );
}

#[test]
fn provenance_and_layout_live_in_their_own_modules() {
    let expected = [
        ("provenance.rs", vec!["fn provenance_lines("]),
        ("layout.rs", vec!["fn wrap_text(", "fn search_layer("]),
        ("fonts.rs", vec!["fn subset_document_font("]),
    ];
    let lib = read("lib.rs");
    let mut problems = Vec::new();
    for (file, defs) in expected {
        let body = read(file);
        for def in defs {
            if !body.contains(def) {
                problems.push(format!("{file} missing `{def}`"));
            }
            if lib.contains(def) {
                problems.push(format!("lib.rs still defines `{def}`"));
            }
        }
    }
    assert!(problems.is_empty(), "layout violations: {problems:?}");
}
