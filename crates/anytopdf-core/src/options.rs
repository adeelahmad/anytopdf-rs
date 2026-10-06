//! Per-plugin option tables resolved from the layered configuration.
//!
//! The configuration file has one top-level table per plugin, such as
//! `[video]` or `[whisper]`. Built-in plugins read their table into a typed
//! struct; runtime plugins receive theirs, by manifest name, verbatim in the
//! `options` field of each request.

use anyhow::{Context, Result};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// The table name a plugin name maps to: `face-detect` and `face_detect` name
/// the same table, so environment variables (which cannot spell `-`) reach it.
pub fn option_table_name(name: &str) -> String {
    name.trim().replace('-', "_")
}

/// Option tables keyed by table name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PluginOptions {
    tables: BTreeMap<String, Map<String, Value>>,
}

impl PluginOptions {
    /// Collects every top-level table of a resolved configuration document;
    /// scalar global settings are ignored.
    pub fn from_document(document: &Value) -> Self {
        let mut options = Self::default();
        for (name, table) in document.as_object().into_iter().flatten() {
            if let Some(table) = table.as_object() {
                options.insert(name, table.clone());
            }
        }
        options
    }

    pub fn insert(&mut self, name: &str, table: Map<String, Value>) {
        self.tables.insert(option_table_name(name), table);
    }

    pub fn table(&self, name: &str) -> Option<&Map<String, Value>> {
        self.tables.get(&option_table_name(name))
    }

    /// Reads a table into `T`, which supplies defaults for missing keys.
    pub fn get<T: DeserializeOwned + Default>(&self, name: &str) -> Result<T> {
        match self.table(name) {
            None => Ok(T::default()),
            Some(table) => serde_json::from_value(Value::Object(table.clone()))
                .with_context(|| format!("invalid [{}] options", option_table_name(name))),
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
    fn tables_are_collected_and_dashes_match_underscores() {
        let doc = serde_json::json!({
            "renderer": "pdfa",
            "json": {"limit": 3},
            "face-detect": {"threshold": 0.5},
        });
        let options = PluginOptions::from_document(&doc);
        assert_eq!(
            options.get::<Sample>("json").unwrap(),
            Sample {
                limit: 3,
                mode: String::new()
            }
        );
        assert!(options.table("face_detect").is_some());
        assert!(options.table("renderer").is_none());
    }

    #[test]
    fn a_missing_table_reads_as_defaults_and_unknown_keys_are_errors() {
        let options = PluginOptions::from_document(&serde_json::json!({
            "json": {"limitt": 3}
        }));
        assert_eq!(options.get::<Sample>("csv").unwrap(), Sample::default());
        let err = options.get::<Sample>("json").unwrap_err();
        assert!(format!("{err:#}").contains("[json]"), "{err:#}");
    }
}
