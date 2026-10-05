use crate::exit::{CliError, ExitClass, fail, tag};
use anytopdf_core::{CHUNKS_SCHEMA_VERSION, Diagnostic, DiagnosticCode, MANIFEST_SCHEMA_VERSION};
use anytopdf_pdf::read_embedded_files;
use serde_json::{Value, json};
use std::path::Path;

const MANIFEST_NAME: &str = "anytopdf-manifest.json";
const CHUNKS_NAME: &str = "anytopdf-chunks.json";

pub fn extract(path: &Path) -> Result<Value, CliError> {
    let pdf = tag(
        ExitClass::Input,
        std::fs::read(path).map_err(|e| anyhow::anyhow!("cannot read {}: {e}", path.display())),
    )?;
    let embedded = tag(ExitClass::Input, read_embedded_files(&pdf))?;
    let find = |name: &str| {
        embedded
            .iter()
            .find(|f| f.name == name)
            .map(|f| f.bytes.clone())
    };
    let (origin, manifest, chunks) = match (find(MANIFEST_NAME), find(CHUNKS_NAME)) {
        (Some(m), Some(c)) => ("embedded", m, c),
        _ => {
            let sidecar = |suffix: &str| {
                let mut name = path.as_os_str().to_owned();
                name.push(format!(".{suffix}.json"));
                std::fs::read(name).ok()
            };
            match (sidecar("manifest"), sidecar("chunks")) {
                (Some(m), Some(c)) => ("sidecar", m, c),
                _ => {
                    return Err(fail(
                        ExitClass::Input,
                        format!("no manifest found in or beside {}", path.display()),
                    ));
                }
            }
        }
    };
    let manifest: Value = tag(ExitClass::Input, serde_json::from_slice(&manifest))?;
    let chunks: Value = tag(ExitClass::Input, serde_json::from_slice(&chunks))?;
    let mut warnings = Vec::new();
    for (label, value, expected) in [
        ("manifest", &manifest, MANIFEST_SCHEMA_VERSION),
        ("chunks", &chunks, CHUNKS_SCHEMA_VERSION),
    ] {
        let found = value["schema_version"].as_str().unwrap_or("");
        if found != expected {
            let d = Diagnostic::new(
                DiagnosticCode::ExtractVersionMismatch,
                format!("{label} schema_version {found:?} != expected {expected:?}"),
            );
            crate::convert::print_diagnostic(&d);
            warnings.push(json!({"code": d.code.as_str(), "message": d.message}));
            continue;
        }
        let text = crate::capabilities::schema_text(label)
            .ok_or_else(|| fail(ExitClass::Internal, format!("no schema for {label}")))?;
        let schema: Value = tag(ExitClass::Internal, serde_json::from_str(text))?;
        let errors = anytopdf_core::schema::validate(&schema, value);
        if let Some(first) = errors.first() {
            let path = if first.path.is_empty() {
                "/"
            } else {
                &first.path
            };
            let more = match errors.len() - 1 {
                0 => String::new(),
                n => format!(" (+{n} more)"),
            };
            return Err(fail(
                ExitClass::Input,
                format!(
                    "{label} does not match {expected}: {path}: {}{more}",
                    first.message
                ),
            ));
        }
    }
    Ok(json!({
        "schema_version": "anytopdf.extract/1",
        "origin": origin,
        "manifest": manifest,
        "chunks": chunks,
        "warnings": warnings,
    }))
}
