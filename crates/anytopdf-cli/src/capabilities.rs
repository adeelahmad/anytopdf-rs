use anyhow::Result;
use anytopdf_core::DiagnosticCode;
use serde_json::{Value, json};

use crate::exit::ExitClass;

const SCHEMAS: [&str; 8] = [
    include_str!("../../../schemas/convert.schema.json"),
    include_str!("../../../schemas/probe.schema.json"),
    include_str!("../../../schemas/doctor.schema.json"),
    include_str!("../../../schemas/plugins.schema.json"),
    include_str!("../../../schemas/capabilities.schema.json"),
    include_str!("../../../schemas/extract.schema.json"),
    include_str!("../../../schemas/manifest.schema.json"),
    include_str!("../../../schemas/chunks.schema.json"),
];

pub fn capabilities() -> Result<Value> {
    let schemas = SCHEMAS
        .iter()
        .map(|text| {
            let schema: Value = serde_json::from_str(text)?;
            Ok(json!({"id": schema["$id"]}))
        })
        .collect::<Result<Vec<_>>>()?;
    let importers: Vec<Value> = ["text", "image", "subtitle", "audio", "ffmpeg-video"]
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
