use super::{Resolved, flags::builtin_tables, user_config_path};
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
        let builtin = builtin_tables();
        let mut tables: Vec<(&String, &Map<String, Value>)> = Vec::new();
        for (key, value) in &self.document {
            match value {
                Value::Object(table) => tables.push((key, table)),
                _ => rows.push(self.row(key, key, value)),
            }
        }
        write_rows(&mut out, &rows, annotate);
        let rank = |name: &String| {
            builtin
                .iter()
                .position(|b| b == name)
                .unwrap_or(builtin.len())
        };
        tables.sort_by(|a, b| rank(a.0).cmp(&rank(b.0)).then_with(|| a.0.cmp(b.0)));
        for (name, table) in tables {
            out.push_str(&format!("\n[{}]", toml_key(name)));
            if annotate && !builtin.contains(name) {
                out.push_str(&format!("  # passed to the runtime plugin named {name}"));
            }
            out.push('\n');
            let rows: Vec<_> = table
                .iter()
                .map(|(k, v)| self.row(k, &format!("{name}.{k}"), v))
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

pub(super) fn toml_value(value: &Value) -> String {
    toml::Value::try_from(value)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| value.to_string())
}
