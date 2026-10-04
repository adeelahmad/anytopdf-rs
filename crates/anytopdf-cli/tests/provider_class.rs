use std::{fs, path::Path};

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn cli_never_matches_ocr_message_text() {
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    assert!(!files.is_empty());
    let offenders: Vec<_> = files
        .iter()
        .filter(|f| {
            fs::read_to_string(f)
                .unwrap()
                .contains("no OCR provider succeeded")
        })
        .collect();
    assert!(offenders.is_empty(), "message match in {offenders:?}");
}
