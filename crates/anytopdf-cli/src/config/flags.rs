//! One definition, three spellings.
//!
//! Every key of a built-in table in `schemas/config-file.schema.json` is an
//! option: `[video] interval` in the file is `ANYTOPDF_VIDEO_INTERVAL` in the
//! environment and `--video-interval` on `anytopdf convert`. The schema's
//! title, description, type and default become the flag's help heading, help
//! text and value parser, so adding a key to the schema adds all three.

use super::values::file_schema;
use clap::{Arg, ArgAction, ArgMatches, Command, builder::PossibleValuesParser};
use serde_json::Value;

/// Older flag names kept as aliases of the generated ones.
const ALIASES: &[(&str, &str)] = &[
    ("ocr.mode", "ocr"),
    ("ocr.lang", "lang"),
    ("video.scene_threshold", "scene-threshold"),
    ("video.dedupe_distance", "dedupe-distance"),
    ("video.max_frames", "max-video-frames"),
    ("image.max_frames", "max-image-frames"),
    ("entities.date_order", "date-order"),
    ("location.mode", "location"),
];

/// One option of a built-in table.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OptionDef {
    pub(crate) table: String,
    pub(crate) key: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) kind: String,
    pub(crate) values: Vec<String>,
    pub(crate) default: Value,
}

impl OptionDef {
    /// `video.interval`
    pub(crate) fn dotted(&self) -> String {
        format!("{}.{}", self.table, self.key)
    }

    /// `video-interval`
    pub(crate) fn flag(&self) -> String {
        format!("{}-{}", self.table, self.key).replace('_', "-")
    }

    /// `ANYTOPDF_VIDEO_INTERVAL`
    pub(crate) fn env(&self) -> String {
        format!("ANYTOPDF_{}_{}", self.table, self.key).to_ascii_uppercase()
    }

    fn arg_id(&self) -> String {
        format!("option:{}", self.dotted())
    }

    fn arg(&self) -> Arg {
        let default = super::render::toml_value(&self.default);
        let mut arg = Arg::new(self.arg_id())
            .long(self.flag())
            .action(ArgAction::Set)
            .value_name(self.key.to_ascii_uppercase())
            .help(format!(
                "{} [default: {default}] [env: {}]",
                self.description.trim_end_matches('.'),
                self.env()
            ))
            .help_heading(format!(
                "{} options ([{}] in the config file)",
                self.title, self.table
            ));
        for (key, alias) in ALIASES {
            if *key == self.dotted() {
                arg = arg.visible_alias(*alias);
            }
        }
        match self.kind.as_str() {
            "boolean" => arg
                .num_args(0..=1)
                .require_equals(true)
                .default_missing_value("true")
                .value_parser(PossibleValuesParser::new(["true", "false"])),
            "integer" => arg.value_parser(clap::value_parser!(i64)),
            "number" => arg.value_parser(clap::value_parser!(f64)),
            _ if !self.values.is_empty() => {
                arg.value_parser(PossibleValuesParser::new(self.values.clone()))
            }
            _ => arg,
        }
    }

    /// The raw text given on the command line, if the flag was used.
    pub(crate) fn given(&self, matches: &ArgMatches) -> Option<String> {
        let id = self.arg_id();
        if matches.value_source(&id) != Some(clap::parser::ValueSource::CommandLine) {
            return None;
        }
        let raw = matches.get_raw(&id)?.next()?;
        Some(raw.to_string_lossy().into_owned())
    }
}

/// Every built-in option, table by table in schema order.
pub(crate) fn builtin_options() -> Vec<OptionDef> {
    let schema = file_schema();
    let mut options = Vec::new();
    for (table, node) in schema["properties"].as_object().into_iter().flatten() {
        let Some(def) = node
            .get("$ref")
            .and_then(Value::as_str)
            .and_then(|r| schema.pointer(r.trim_start_matches('#')))
        else {
            continue;
        };
        let title = def["title"].as_str().unwrap_or(table).to_string();
        for (key, property) in def["properties"].as_object().into_iter().flatten() {
            let values: Vec<String> = property["enum"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();
            options.push(OptionDef {
                table: table.clone(),
                key: key.clone(),
                title: title.clone(),
                description: property["description"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                kind: property["type"].as_str().unwrap_or("string").to_string(),
                values,
                default: property["default"].clone(),
            });
        }
    }
    options
}

/// Built-in table names, in schema order.
pub(crate) fn builtin_tables() -> Vec<String> {
    let mut tables: Vec<String> = Vec::new();
    for option in builtin_options() {
        if !tables.contains(&option.table) {
            tables.push(option.table);
        }
    }
    tables
}

/// Adds one flag per built-in option to `convert`.
pub(crate) fn augment(command: Command) -> Command {
    command.mut_subcommand("convert", |convert| {
        convert.args(builtin_options().iter().map(OptionDef::arg))
    })
}
