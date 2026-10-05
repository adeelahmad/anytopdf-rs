use std::{fs, path::PathBuf};

pub fn repo_file(name: &str) -> String {
    let root: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .find(|dir| dir.join("Cargo.toml").is_file() && dir.join("crates").is_dir())
        .expect("workspace root")
        .to_path_buf();
    fs::read_to_string(root.join(name)).unwrap_or_else(|err| panic!("read {name}: {err}"))
}
