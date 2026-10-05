use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub fn events_schema() -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root
        .ancestors()
        .map(|dir| dir.join("schemas").join("events.schema.json"))
        .find(|candidate| candidate.is_file())
        .expect("schemas/events.schema.json must exist");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

pub fn base(dir: &Path, inputs: &[&str], extra: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    cmd.current_dir(dir)
        .arg("--no-plugins")
        .env_remove("SOURCE_DATE_EPOCH")
        .arg("convert")
        .args(inputs)
        .args(extra);
    cmd
}
