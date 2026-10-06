use super::FILE_SCHEMA;
use anyhow::{Result, anyhow, bail};
use serde_json::Value;

pub(super) fn key_path(key: &str) -> Result<Vec<String>> {
    let path: Vec<String> = key.split('.').map(|s| s.trim().to_string()).collect();
    if key.is_empty() || path.iter().any(String::is_empty) {
        bail!("{key:?} is not a dotted key such as video.interval");
    }
    Ok(path)
}

pub(super) fn pointer_key(pointer: &str) -> String {
    pointer
        .split('/')
        .skip(1)
        .map(|s| s.replace("~1", "/").replace("~0", "~"))
        .collect::<Vec<_>>()
        .join(".")
}

pub(super) fn file_schema() -> Value {
    serde_json::from_str(FILE_SCHEMA).expect("config-file schema is JSON")
}

/// The file schema's node for `path`, following `$ref`s and plugin tables.
pub(super) fn schema_node(path: &[String]) -> Option<Value> {
    let schema = file_schema();
    let resolve = |node: &Value| -> Option<Value> {
        match node.get("$ref").and_then(Value::as_str) {
            Some(r) => schema.pointer(r.trim_start_matches('#')).cloned(),
            None => Some(node.clone()),
        }
    };
    let mut node = schema.clone();
    for segment in path {
        let next = node
            .get("properties")
            .and_then(|p| p.get(segment))
            .or_else(|| node.get("additionalProperties").filter(|a| a.is_object()))
            .cloned()?;
        node = resolve(&next)?;
    }
    Some(node)
}

/// The JSON type the file schema declares at `path`, if it names one.
pub(super) fn expected_type(path: &[String]) -> Option<String> {
    let node = schema_node(path)?;
    if let Some(kind) = node.get("type").and_then(Value::as_str) {
        return Some(kind.to_string());
    }
    node.get("enum").map(|_| "string".to_string())
}

/// `; expected one of: …` for a key whose schema lists its values.
pub(super) fn allowed_values(key: &str) -> String {
    let path: Vec<String> = key.split('.').map(str::to_string).collect();
    let listed = schema_node(&path).and_then(|node| {
        let values = node.get("enum")?.as_array()?.iter();
        Some(
            values
                .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_string))
                .collect::<Vec<_>>(),
        )
    });
    listed.map_or_else(String::new, |values| {
        format!("; expected one of {}", values.join(", "))
    })
}

/// Parses a value given as text (environment variable or `--set`) using the
/// type the schema declares for `path`. Keys the schema does not type, such
/// as runtime plugin options, take a TOML literal and fall back to a string.
pub(super) fn parse_value(path: &[String], raw: &str) -> Result<Value> {
    let raw_trim = raw.trim();
    match expected_type(path).as_deref() {
        Some("boolean") => match raw_trim.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" | "on" => Ok(Value::Bool(true)),
            "false" | "0" | "no" | "off" => Ok(Value::Bool(false)),
            _ => bail!("expected true or false, got {raw:?}"),
        },
        Some("integer") => raw_trim
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| anyhow!("expected an integer, got {raw:?}")),
        Some("number") => raw_trim
            .parse::<i64>()
            .map(Value::from)
            .or_else(|_| {
                raw_trim
                    .parse::<f64>()
                    .ok()
                    .and_then(serde_json::Number::from_f64)
                    .map(Value::Number)
                    .ok_or(())
            })
            .map_err(|_| anyhow!("expected a number, got {raw:?}")),
        Some("array") if raw_trim.starts_with('[') => {
            toml_literal(raw_trim).ok_or_else(|| anyhow!("expected a TOML array, got {raw:?}"))
        }
        Some("array") => Ok(Value::Array(
            raw.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(|s| Value::String(s.to_string()))
                .collect(),
        )),
        Some(_) => Ok(Value::String(raw.to_string())),
        None => Ok(toml_literal(raw_trim).unwrap_or_else(|| Value::String(raw.to_string()))),
    }
}

pub(super) fn toml_literal(raw: &str) -> Option<Value> {
    let table: toml::Table = toml::from_str(&format!("v = {raw}")).ok()?;
    serde_json::to_value(table.get("v")?).ok()
}

/// A flag's values as the configuration value for `path`.
pub(super) fn flag_value(path: &[String], raws: Vec<String>) -> Value {
    if expected_type(path).as_deref() == Some("array") {
        return Value::Array(raws.into_iter().map(Value::String).collect());
    }
    let raw = raws.into_iter().next().unwrap_or_default();
    parse_value(path, &raw).unwrap_or(Value::String(raw))
}
