use std::fs;
use std::path::{Path, PathBuf};

fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn cli_source_files_stay_under_800_lines() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    assert!(
        files.len() >= 5,
        "expected at least 5 source files, found {}",
        files.len()
    );
    let offenders: Vec<String> = files
        .iter()
        .filter_map(|f| {
            let n = fs::read_to_string(f).expect("read source").lines().count();
            (n >= 800).then(|| format!("{} has {n} lines", f.display()))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "files must stay under 800 lines:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn main_rs_keeps_only_entry_point_and_dispatch() {
    let main = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
    let text = fs::read_to_string(main).expect("read main.rs");
    for needed in ["fn main(", "fn run("] {
        assert!(text.contains(needed), "main.rs must contain `{needed}`");
    }
    let moved: Vec<&str> = [
        "fn convert_inner(",
        "fn stage_document(",
        "fn doctor(",
        "fn probe(",
        "fn publish_output(",
        "struct ConvertArgs",
    ]
    .into_iter()
    .filter(|s| text.contains(s))
    .collect();
    assert!(moved.is_empty(), "main.rs must not contain: {moved:?}");
}
