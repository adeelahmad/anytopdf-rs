//! Per-type option tables resolved from the layered configuration.
//!
//! The configuration file has one table per importer, enricher and renderer,
//! such as `[importer.video]` or `[enricher.whisper]`. Built-in plugins read
//! their table into a typed struct; runtime plugins receive theirs verbatim in
//! the `options` field of each request.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The configuration sections that hold one table per plugin.
pub const OPTION_SECTIONS: [&str; 3] = ["importer", "enricher", "renderer"];

/// The configuration section for a plugin capability kind:
/// `importer`, `enricher` (source, graph and unit enrichers) or `renderer`.
pub fn option_section(kind: &str) -> Option<&'static str> {
    match kind {
        "importer" => Some("importer"),
        "source-enricher" | "graph-enricher" | "unit-enricher" => Some("enricher"),
        "renderer" => Some("renderer"),
        _ => None,
    }
}

/// The table name a plugin name maps to: `face-detect` and `face_detect` name
/// the same table, so environment variables (which cannot spell `-`) reach it.
pub fn option_table_name(name: &str) -> String {
    name.trim().replace('-', "_")
}

/// Option tables keyed by section and table name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginOptions {
    tables: BTreeMap<(String, String), Map<String, Value>>,
}

impl PluginOptions {
    /// Collects the tables under `importer`, `enricher` and `renderer` from a
    /// resolved configuration document; other keys are ignored.
    pub fn from_document(document: &Value) -> Self {
        let mut options = Self::default();
        for section in OPTION_SECTIONS {
            let Some(tables) = document.get(section).and_then(Value::as_object) else {
                continue;
            };
            for (name, table) in tables {
                if let Some(table) = table.as_object() {
                    options.insert(section, name, table.clone());
                }
            }
        }
        options
    }

    pub fn insert(&mut self, section: &str, name: &str, table: Map<String, Value>) {
        self.tables
            .insert((section.to_string(), option_table_name(name)), table);
    }

    pub fn table(&self, section: &str, name: &str) -> Option<&Map<String, Value>> {
        self.tables
            .get(&(section.to_string(), option_table_name(name)))
    }

    /// The table for a plugin with capability `kind` and manifest `name`.
    pub fn for_kind(&self, kind: &str, name: &str) -> Option<&Map<String, Value>> {
        self.table(option_section(kind)?, name)
    }

    /// Reads a table into `T`, which supplies defaults for missing keys.
    pub fn get<T: DeserializeOwned + Default>(&self, section: &str, name: &str) -> Result<T> {
        match self.table(section, name) {
            None => Ok(T::default()),
            Some(table) => {
                serde_json::from_value(Value::Object(table.clone())).with_context(|| {
                    format!("invalid [{section}.{}] options", option_table_name(name))
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Default, PartialEq, Deserialize)]
    #[serde(default, deny_unknown_fields)]
    struct Sample {
        limit: u32,
        mode: String,
    }

    #[test]
    fn tables_are_collected_per_section_and_dashes_match_underscores() {
        let doc = serde_json::json!({
            "renderer": "pdfa",
            "importer": {"json": {"limit": 3}},
            "enricher": {"face-detect": {"threshold": 0.5}},
        });
        let options = PluginOptions::from_document(&doc);
        assert_eq!(
            options.get::<Sample>("importer", "json").unwrap(),
            Sample {
                limit: 3,
                mode: String::new()
            }
        );
        assert!(options.for_kind("unit-enricher", "face_detect").is_some());
        assert!(options.for_kind("importer", "face-detect").is_none());
        assert!(options.table("renderer", "pdfa").is_none());
    }

    #[test]
    fn a_missing_table_reads_as_defaults_and_unknown_keys_are_errors() {
        let options = PluginOptions::from_document(&serde_json::json!({
            "importer": {"json": {"limitt": 3}}
        }));
        assert_eq!(
            options.get::<Sample>("importer", "csv").unwrap(),
            Sample::default()
        );
        let err = options.get::<Sample>("importer", "json").unwrap_err();
        assert!(format!("{err:#}").contains("[importer.json]"), "{err:#}");
    }
}
