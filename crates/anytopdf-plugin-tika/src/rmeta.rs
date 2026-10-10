//! Turns Tika's recursive metadata list into protocol units: one text unit
//! for the file and one for each embedded document with text, plus the
//! file's own metadata as `tika.*` source metadata.

use anyhow::{Result, bail};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

const CONTENT: &str = "X-TIKA:content";
/// Text kept across all units, so the response stays well inside the host's
/// 16 MiB limit even after JSON escaping.
const MAX_TEXT_BYTES: usize = 8 * 1024 * 1024;
const MAX_METADATA_ENTRIES: usize = 200;
const MAX_METADATA_CHARS: usize = 1000;

pub struct Import {
    pub units: Vec<Value>,
    pub metadata: BTreeMap<String, String>,
    pub content_type: Option<String>,
}

pub fn units(documents: &[Value], name: &str, warnings: &mut Vec<String>) -> Result<Import> {
    let Some(container) = documents.first().and_then(Value::as_object) else {
        bail!("Tika returned nothing for {name}");
    };
    let content_type = field(container, "Content-Type").map(|t| base_type(&t));
    let mut budget = MAX_TEXT_BYTES;
    let mut truncated = false;
    let mut units = Vec::new();
    for (index, document) in documents.iter().enumerate() {
        let Some(document) = document.as_object() else {
            continue;
        };
        for (key, value) in document {
            if key.starts_with("X-TIKA:EXCEPTION:") || key == "X-TIKA:WARN" {
                let message = text_of(value);
                let first = message.lines().next().unwrap_or_default().trim();
                if !first.is_empty() {
                    warnings.push(format!("tika: {name}: {first}"));
                }
            }
        }
        let mut text = tidy(&field(document, CONTENT).unwrap_or_default());
        if text.is_empty() {
            continue;
        }
        if budget == 0 {
            truncated = true;
            break;
        }
        if text.len() > budget {
            let mut cut = budget;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            truncated = true;
        }
        budget -= text.len();
        let mut metadata = Map::new();
        if let Some(kind) = field(document, "Content-Type") {
            metadata.insert("tika.content-type".into(), json!(base_type(&kind)));
        }
        if index > 0 {
            let member = field(document, "X-TIKA:embedded_resource_path")
                .map(|p| p.trim_start_matches('/').to_string())
                .filter(|p| !p.is_empty())
                .or_else(|| field(document, "resourceName"))
                .unwrap_or_else(|| format!("embedded-{index}"));
            metadata.insert("container.member".into(), json!(member));
            if let Some(kind) = field(document, "Content-Type") {
                metadata.insert("container.member-type".into(), json!(base_type(&kind)));
            }
        }
        units.push(json!({
            "kind": "text",
            "visual_path": null,
            "visible_text": text,
            "time_range": null,
            "annotations": [],
            "metadata": metadata,
        }));
    }
    if truncated {
        warnings.push(format!(
            "tika: {name}: text truncated to {} MiB",
            MAX_TEXT_BYTES / (1024 * 1024)
        ));
    }
    if units.is_empty() {
        bail!(
            "Tika found no text in {name} ({})",
            content_type.as_deref().unwrap_or("unknown type")
        );
    }
    let metadata = container
        .iter()
        .filter(|(key, _)| {
            // Tika's own bookkeeping (content, parsers, timings, errors).
            !key.starts_with("X-TIKA:")
        })
        .filter_map(|(key, value)| {
            let text: String = text_of(value).chars().take(MAX_METADATA_CHARS).collect();
            (!text.trim().is_empty()).then(|| (format!("tika.{key}"), text))
        })
        .take(MAX_METADATA_ENTRIES)
        .collect();
    Ok(Import {
        units,
        metadata,
        content_type,
    })
}

fn field(document: &Map<String, Value>, key: &str) -> Option<String> {
    document
        .get(key)
        .map(text_of)
        .filter(|s| !s.trim().is_empty())
}

/// Tika writes repeated metadata values as arrays.
fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(items) => items.iter().map(text_of).collect::<Vec<_>>().join("; "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `text/plain; charset=UTF-8` -> `text/plain`.
fn base_type(content_type: &str) -> String {
    content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

/// Strips trailing spaces and collapses Tika's runs of blank lines to one.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = false;
    for line in text.lines().map(str::trim_end) {
        if line.trim().is_empty() {
            blank = !out.is_empty();
            continue;
        }
        if blank {
            out.push('\n');
            blank = false;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_and_embedded_documents_become_text_units() {
        let documents = vec![
            json!({
                "Content-Type": "application/epub+zip",
                "dc:title": "A Book",
                "dc:creator": ["Ann", "Bo"],
                "X-TIKA:Parsed-By": ["org.apache.tika.parser.DefaultParser"],
                "X-TIKA:parse_time_millis": "62",
                "X-TIKA:content": "\n\nChapter one.  \n\n\n\nThe end.\n\n"
            }),
            json!({
                "Content-Type": "text/plain; charset=UTF-8",
                "resourceName": "notes.txt",
                "X-TIKA:embedded_resource_path": "/OEBPS/notes.txt",
                "X-TIKA:content": "Embedded notes"
            }),
            json!({"Content-Type": "image/png", "X-TIKA:content": "  \n"}),
        ];
        let mut warnings = Vec::new();
        let import = units(&documents, "book.epub", &mut warnings).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(import.units.len(), 2, "the image had no text");
        assert_eq!(import.units[0]["visible_text"], "Chapter one.\n\nThe end.");
        assert_eq!(
            import.units[0]["metadata"]["tika.content-type"],
            "application/epub+zip"
        );
        assert!(import.units[0]["metadata"]["container.member"].is_null());
        assert_eq!(
            import.units[1]["metadata"]["container.member"],
            "OEBPS/notes.txt"
        );
        assert_eq!(
            import.units[1]["metadata"]["container.member-type"],
            "text/plain"
        );
        assert_eq!(import.content_type.as_deref(), Some("application/epub+zip"));
        assert_eq!(import.metadata["tika.dc:title"], "A Book");
        assert_eq!(import.metadata["tika.dc:creator"], "Ann; Bo");
        assert!(!import.metadata.contains_key("tika.X-TIKA:content"));
        assert!(!import.metadata.contains_key("tika.X-TIKA:Parsed-By"));
        assert!(
            !import
                .metadata
                .contains_key("tika.X-TIKA:parse_time_millis")
        );
    }

    #[test]
    fn no_text_is_an_error_naming_the_type() {
        let documents = vec![json!({"Content-Type": "application/octet-stream"})];
        let err = units(&documents, "blob.bin", &mut Vec::new())
            .err()
            .unwrap();
        assert_eq!(
            err.to_string(),
            "Tika found no text in blob.bin (application/octet-stream)"
        );
        assert!(units(&[], "x", &mut Vec::new()).is_err());
    }

    #[test]
    fn tika_exceptions_become_warnings() {
        let documents = vec![json!({
            "X-TIKA:content": "partial",
            "X-TIKA:EXCEPTION:embedded_exception": "java.io.IOException: bad stream\n\tat x.y"
        })];
        let mut warnings = Vec::new();
        units(&documents, "mail.msg", &mut warnings).unwrap();
        assert_eq!(
            warnings,
            vec!["tika: mail.msg: java.io.IOException: bad stream"]
        );
    }

    #[test]
    fn oversized_text_is_truncated_on_a_char_boundary() {
        let big = "é".repeat(MAX_TEXT_BYTES / 2 + 10);
        let documents = vec![
            json!({"X-TIKA:content": big}),
            json!({"X-TIKA:content": "dropped"}),
        ];
        let mut warnings = Vec::new();
        let import = units(&documents, "huge.epub", &mut warnings).unwrap();
        assert_eq!(import.units.len(), 1);
        let text = import.units[0]["visible_text"].as_str().unwrap();
        assert!(text.len() <= MAX_TEXT_BYTES);
        assert!(warnings[0].contains("truncated"));
    }
}
