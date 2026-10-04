use std::fs;
use std::path::Path;

#[test]
fn builtin_sources_use_the_shared_basename_helper() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    for rel in [
        "src/metadata.rs",
        "src/importers/image_file.rs",
        "src/importers/audio.rs",
        "src/importers/text.rs",
    ] {
        let src = fs::read_to_string(root.join(rel)).unwrap();
        let production = src.split("#[cfg(test)]").next().unwrap();
        if production.contains("file_name()") {
            offenders.push(format!("{rel}: still contains file_name()"));
        }
        if !production.contains("basename(") {
            offenders.push(format!("{rel}: does not call basename("));
        }
    }
    assert!(offenders.is_empty(), "{}", offenders.join("\n"));
}
