use std::{fs, path::PathBuf};

use anytopdf_core::schema;
use serde_json::Value;

pub fn load_schema(name: &str) -> Value {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = root
        .ancestors()
        .map(|dir| dir.join("schemas").join(format!("{name}.schema.json")))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("schemas/{name}.schema.json must exist"));
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

pub fn validation_errors(name: &str, instance: &Value) -> Vec<String> {
    schema::validate(&load_schema(name), instance)
        .into_iter()
        .map(|error| format!("{}: {}", error.path, error.message))
        .collect()
}
