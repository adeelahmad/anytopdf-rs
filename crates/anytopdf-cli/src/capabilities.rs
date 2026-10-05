use anyhow::Result;
use anytopdf_core::DiagnosticCode;
use serde_json::{Value, json};

use crate::exit::ExitClass;

const SCHEMAS: [(&str, &str); 9] = [
    (
        "convert",
        include_str!("../../../schemas/convert.schema.json"),
    ),
    ("probe", include_str!("../../../schemas/probe.schema.json")),
    (
        "doctor",
        include_str!("../../../schemas/doctor.schema.json"),
    ),
    (
        "plugins",
        include_str!("../../../schemas/plugins.schema.json"),
    ),
    (
        "capabilities",
        include_str!("../../../schemas/capabilities.schema.json"),
    ),
    (
        "extract",
        include_str!("../../../schemas/extract.schema.json"),
    ),
    (
        "manifest",
        include_str!("../../../schemas/manifest.schema.json"),
    ),
    (
        "chunks",
        include_str!("../../../schemas/chunks.schema.json"),
    ),
    (
        "events",
        include_str!("../../../schemas/events.schema.json"),
    ),
];

pub(crate) fn schema_text(name: &str) -> Option<&'static str> {
    SCHEMAS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, text)| *text)
}

pub fn capabilities() -> Result<Value> {
    let schemas = SCHEMAS
        .iter()
        .map(|(_, text)| {
            let schema: Value = serde_json::from_str(text)?;
            Ok(json!({"id": schema["$id"]}))
        })
        .collect::<Result<Vec<_>>>()?;
    let importers: Vec<Value> = ["text", "html", "image", "subtitle", "audio", "ffmpeg-video"]
        .iter()
        .map(|name| json!({"name": name}))
        .collect();
    Ok(json!({
        "schema_version": "anytopdf.capabilities/1",
        "exit_codes": ExitClass::ALL
            .iter()
            .map(|c| json!({"code": c.code(), "name": c.name()}))
            .collect::<Vec<_>>(),
        "diagnostic_codes": DiagnosticCode::ALL
            .iter()
            .map(|c| json!({
                "code": c.as_str(),
                "severity": format!("{:?}", c.severity()).to_lowercase(),
            }))
            .collect::<Vec<_>>(),
        "profiles": ["archive", "share"],
        "ocr_modes": ["auto", "vision", "doctr", "tesseract", "off"],
        "importers": importers,
        "schemas": schemas,
    }))
}
