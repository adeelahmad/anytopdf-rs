use super::{Resolved, as_object, user_config_path};
use anytopdf_core::OPTION_SECTIONS;
use serde_json::{Map, Value};

impl Resolved {
    /// The effective configuration as TOML, annotated with each value's origin.
    pub(crate) fn render(&self, annotate: bool) -> String {
        let mut out = String::new();
        if annotate {
            out.push_str(
                "# Effective anytopdf configuration; each value notes where it came from.\n",
            );
            if self.files.is_empty() {
                out.push_str("# No configuration file was read.\n");
            }
            for file in &self.files {
                let state = if file.loaded { "read" } else { "not found" };
                out.push_str(&format!("# file: {} ({state})\n", file.path.display()));
            }
        } else {
            out.push_str("# anytopdf configuration with every built-in default.\n");
            if let Some(path) = user_config_path() {
                out.push_str(&format!(
                    "# Save as {} or pass --config PATH.\n",
                    path.display()
                ));
            }
            out.push_str("# filter = \"invoice|receipt\"\n# plugin_sandbox = \"contain\"\n");
        }
        let mut rows: Vec<(String, String, String)> = Vec::new();
        // (header, rank, entries): a section's own keys such as `renderer.use`
        // come before its plugin tables.
        let mut tables: Vec<(String, usize, Map<String, Value>)> = Vec::new();
        for (key, value) in &self.document {
            let section = OPTION_SECTIONS.iter().position(|s| s == key);
            match (section, value) {
                (Some(rank), Value::Object(entries)) => {
                    let (own, plugins): (Map<_, _>, Map<_, _>) = entries
                        .clone()
                        .into_iter()
                        .partition(|(_, v)| !v.is_object());
                    if !own.is_empty() {
                        tables.push((key.clone(), rank * 2, own));
                    }
                    for (name, table) in plugins {
                        tables.push((
                            format!("{key}.{}", toml_key(&name)),
                            rank * 2 + 1,
                            as_object(table),
                        ));
                    }
                }
                _ => rows.push(self.row(key, key, value)),
            }
        }
        write_rows(&mut out, &rows, annotate);
        tables.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        for (header, _, table) in tables {
            out.push_str(&format!("\n[{header}]\n"));
            let rows: Vec<_> = table
                .iter()
                .map(|(k, v)| self.row(k, &format!("{}.{k}", header.replace('"', "")), v))
                .collect();
            write_rows(&mut out, &rows, annotate);
        }
        out
    }

    fn row(&self, key: &str, path: &str, value: &Value) -> (String, String, String) {
        let origin = self
            .origin_under(path)
            .map(ToString::to_string)
            .unwrap_or_default();
        (toml_key(key), toml_value(value), origin)
    }
}

fn write_rows(out: &mut String, rows: &[(String, String, String)], annotate: bool) {
    let width = rows
        .iter()
        .map(|(k, v, _)| k.len() + v.len() + 3)
        .max()
        .unwrap_or(0);
    for (key, value, origin) in rows {
        let line = format!("{key} = {value}");
        if annotate {
            out.push_str(&format!("{line:<width$}  # {origin}\n"));
        } else {
            out.push_str(&line);
            out.push('\n');
        }
    }
}

fn toml_key(key: &str) -> String {
    if !key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        key.to_string()
    } else {
        Value::String(key.to_string()).to_string()
    }
}

fn toml_value(value: &Value) -> String {
    toml::Value::try_from(value)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| value.to_string())
}
