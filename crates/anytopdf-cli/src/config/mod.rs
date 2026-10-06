//! Layered configuration.
//!
//! Each layer overrides the one before it: built-in defaults, the user config
//! file (or the files named by `--config`), `ANYTOPDF_*` environment variables,
//! command-line flags, and `--set KEY=VALUE`. The merged document has the shape
//! of `schemas/config-file.schema.json`: global keys at the top level and one
//! table per plugin under `importer`, `enricher` and `renderer`.

use crate::cli::{Cli, Commands};
use anyhow::{Context, Result, anyhow, bail};
use anytopdf_builtin::{BuiltinOptions, option_tables};
use anytopdf_core::{OPTION_SECTIONS, PluginOptions, SandboxMode, option_table_name, schema};
use clap::{ArgMatches, parser::ValueSource};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
mod render;
#[cfg(test)]
mod tests;
mod values;

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
};
use values::{allowed_values, file_schema, flag_value, key_path, parse_value, pointer_key};

pub(crate) const SCHEMA_VERSION: &str = "anytopdf.config/1";
pub(crate) const HELP_FOOTER: &str = "\
Layers, later ones winning:
  1. built-in defaults
  2. the user config file (~/.config/anytopdf/config.toml, or %APPDATA%\\anytopdf\\config.toml
     on Windows), or the files named by --config or ANYTOPDF_CONFIG instead
  3. environment variables: ANYTOPDF_<KEY> for top-level keys (ANYTOPDF_PROFILE=share)
     and ANYTOPDF_<SECTION>__<TABLE>__<KEY> for tables
     (ANYTOPDF_IMPORTER__VIDEO__INTERVAL=2)
  4. command-line flags (--video-interval 2)
  5. --set KEY=VALUE (--set importer.video.interval=2)

Top-level keys are global settings; [importer.NAME], [enricher.NAME] and
[renderer.NAME] hold one table per plugin, and runtime plugins receive theirs
in each request. Start from `anytopdf config --defaults`. Schema:
schemas/config-file.schema.json.";
const FILE_SCHEMA: &str = include_str!("../../../../schemas/config-file.schema.json");
const ENV_PREFIX: &str = "ANYTOPDF_";
/// Path list (platform separator) used instead of the user config file.
pub(crate) const CONFIG_ENV: &str = "ANYTOPDF_CONFIG";
/// Any non-empty value ignores every configuration file.
pub(crate) const NO_CONFIG_ENV: &str = "ANYTOPDF_NO_CONFIG";

/// Where an effective value came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum Origin {
    Default,
    File { path: PathBuf },
    Env { name: String },
    Flag { flag: String },
    Set,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Origin::Default => f.write_str("default"),
            Origin::File { path } => write!(f, "{}", path.display()),
            Origin::Env { name } => write!(f, "env {name}"),
            Origin::Flag { flag } => f.write_str(flag),
            Origin::Set => f.write_str("--set"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct FileStatus {
    pub(crate) path: PathBuf,
    pub(crate) loaded: bool,
    pub(crate) explicit: bool,
}

/// A command-line flag that also has a configuration key.
struct Binding {
    arg: &'static str,
    flag: &'static str,
    key: &'static str,
    /// A `--no-…` switch that sets the key to `false`.
    negates: bool,
}

const fn bind(arg: &'static str, flag: &'static str, key: &'static str) -> Binding {
    Binding {
        arg,
        flag,
        key,
        negates: false,
    }
}

const fn negate(arg: &'static str, flag: &'static str, key: &'static str) -> Binding {
    Binding {
        arg,
        flag,
        key,
        negates: true,
    }
}

const GLOBAL_FLAGS: &[Binding] = &[
    negate("no_plugins", "--no-plugins", "plugins"),
    bind("plugin_timeout", "--plugin-timeout", "plugin_timeout"),
    bind(
        "allow_plugin_kind",
        "--allow-plugin-kind",
        "allow_plugin_kind",
    ),
    bind("deny_plugin_kind", "--deny-plugin-kind", "deny_plugin_kind"),
    bind("plugin_sandbox", "--plugin-sandbox", "plugin_sandbox"),
    bind(
        "plugin_sandbox_allow_read",
        "--plugin-sandbox-allow-read",
        "plugin_sandbox_allow_read",
    ),
];

const CONVERT_FLAGS: &[Binding] = &[
    bind("strict", "--strict", "strict"),
    bind("fail_fast", "--fail-fast", "fail_fast"),
    bind("renderer", "--renderer", "renderer.use"),
    bind("filter", "--filter", "filter"),
    bind("include_hidden", "--include-hidden", "include_hidden"),
    bind("profile", "--profile", "profile"),
    negate(
        "no_provenance_page",
        "--no-provenance-page",
        "provenance_page",
    ),
    bind("ocr", "--ocr", "enricher.ocr.mode"),
    bind("lang", "--lang", "enricher.ocr.lang"),
    bind(
        "video_interval",
        "--video-interval",
        "importer.video.interval",
    ),
    bind(
        "scene_threshold",
        "--scene-threshold",
        "importer.video.scene_threshold",
    ),
    bind(
        "dedupe_distance",
        "--dedupe-distance",
        "importer.video.dedupe_distance",
    ),
    bind(
        "max_video_frames",
        "--max-video-frames",
        "importer.video.max_frames",
    ),
    bind(
        "max_image_frames",
        "--max-image-frames",
        "importer.image.max_frames",
    ),
    negate(
        "no_embedded_subtitles",
        "--no-embedded-subtitles",
        "enricher.captions.embedded_subtitles",
    ),
];

/// Global settings with defaults; must match the clap defaults in `cli.rs`.
fn global_defaults() -> Value {
    json!({
        "profile": "archive",
        "strict": false,
        "fail_fast": false,
        "include_hidden": false,
        "provenance_page": true,
        "plugins": true,
        "plugin_timeout": 60,
        "allow_plugin_kind": [],
        "deny_plugin_kind": [],
        "plugin_sandbox_allow_read": [],
        "renderer": {"use": "pdfa"},
    })
}

/// The merged configuration and where each value came from.
#[derive(Debug, Clone)]
pub(crate) struct Resolved {
    pub(crate) document: Map<String, Value>,
    pub(crate) origins: BTreeMap<String, Origin>,
    pub(crate) files: Vec<FileStatus>,
}

/// Everything outside the process arguments that configuration reads.
pub(crate) struct Environment {
    pub(crate) vars: Vec<(OsString, OsString)>,
    pub(crate) user_file: Option<PathBuf>,
}

impl Environment {
    pub(crate) fn current() -> Self {
        let mut vars: Vec<_> = std::env::vars_os().collect();
        vars.sort();
        Self {
            vars,
            user_file: user_config_path(),
        }
    }

    fn var(&self, name: &str) -> Option<&OsString> {
        self.vars.iter().find(|(k, _)| k == name).map(|(_, v)| v)
    }
}

/// `%APPDATA%\anytopdf\config.toml` on Windows, otherwise
/// `$XDG_CONFIG_HOME/anytopdf/config.toml` or `~/.config/anytopdf/config.toml`.
pub(crate) fn user_config_path() -> Option<PathBuf> {
    let absolute = |var: &str| {
        std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    };
    let base = if cfg!(windows) {
        absolute("APPDATA")?
    } else {
        absolute("XDG_CONFIG_HOME").or_else(|| absolute("HOME").map(|h| h.join(".config")))?
    };
    Some(base.join("anytopdf").join("config.toml"))
}

impl Resolved {
    /// Built-in defaults only.
    pub(crate) fn defaults() -> Self {
        let mut resolved = Self {
            document: Map::new(),
            origins: BTreeMap::new(),
            files: Vec::new(),
        };
        let mut defaults = global_defaults();
        for table in option_tables() {
            defaults[table.section][table.name] = table.defaults;
        }
        resolved.merge(as_object(defaults), &Origin::Default);
        resolved
    }

    /// Resolves every layer for this invocation and validates the result.
    pub(crate) fn load(cli: &Cli, matches: &ArgMatches, env: &Environment) -> Result<Self> {
        let mut resolved = Self::defaults();
        for file in config_files(cli, env) {
            let loaded = file.path.is_file() || file.explicit;
            if loaded {
                let layer = read_file(&file.path)?;
                resolved.merge(
                    layer,
                    &Origin::File {
                        path: file.path.clone(),
                    },
                );
            }
            resolved.files.push(FileStatus { loaded, ..file });
        }
        resolved.apply_env(env)?;
        resolved.apply_flags(matches);
        for item in &cli.set {
            let (key, raw) = item
                .split_once('=')
                .ok_or_else(|| anyhow!("--set {item:?} must look like KEY=VALUE"))?;
            let path = key_path(key.trim())
                .with_context(|| format!("--set {item:?} has an invalid key"))?;
            let value =
                parse_value(&path, raw).with_context(|| format!("--set {}", path.join(".")))?;
            resolved.set(&path, value, &Origin::Set);
        }
        resolved.validate()?;
        Ok(resolved)
    }

    fn merge(&mut self, layer: Map<String, Value>, origin: &Origin) {
        merge_into(&mut self.document, layer, "", origin, &mut self.origins);
    }

    fn set(&mut self, path: &[String], value: Value, origin: &Origin) {
        let mut layer = value;
        for segment in path.iter().rev() {
            layer = json!({ segment.as_str(): layer });
        }
        self.merge(as_object(layer), origin);
    }

    fn apply_env(&mut self, env: &Environment) -> Result<()> {
        let schema = file_schema();
        let globals = schema["properties"].as_object().expect("schema properties");
        for (name, value) in &env.vars {
            let Some(name) = name.to_str() else { continue };
            let Some(rest) = name.strip_prefix(ENV_PREFIX) else {
                continue;
            };
            let path: Vec<String> = if rest.contains("__") {
                rest.split("__").map(str::to_ascii_lowercase).collect()
            } else {
                vec![rest.to_ascii_lowercase()]
            };
            // Other ANYTOPDF_* variables (secrets, IMAP, plugin path) are not settings.
            let first = path[0].as_str();
            let known = if path.len() == 1 {
                globals.contains_key(first)
                    && (first == "renderer" || !OPTION_SECTIONS.contains(&first))
            } else {
                OPTION_SECTIONS.contains(&first) && path.iter().all(|s| !s.is_empty())
            };
            if !known {
                continue;
            }
            let raw = value
                .to_str()
                .ok_or_else(|| anyhow!("{name} is not valid UTF-8"))?;
            let parsed = parse_value(&path, raw).with_context(|| name.to_string())?;
            self.set(
                &path,
                parsed,
                &Origin::Env {
                    name: name.to_string(),
                },
            );
        }
        Ok(())
    }

    fn apply_flags(&mut self, matches: &ArgMatches) {
        let mut groups = vec![(matches, GLOBAL_FLAGS)];
        if let Some(convert) = matches.subcommand_matches("convert") {
            groups.push((convert, CONVERT_FLAGS));
        }
        for (matches, bindings) in groups {
            for binding in bindings {
                if matches.value_source(binding.arg) != Some(ValueSource::CommandLine) {
                    continue;
                }
                let path = key_path(binding.key).expect("binding key");
                let value = if binding.negates {
                    Value::Bool(false)
                } else {
                    let raws: Vec<String> = matches
                        .get_raw(binding.arg)
                        .into_iter()
                        .flatten()
                        .map(|raw| raw.to_string_lossy().into_owned())
                        .collect();
                    flag_value(&path, raws)
                };
                self.set(
                    &path,
                    value,
                    &Origin::Flag {
                        flag: binding.flag.to_string(),
                    },
                );
            }
        }
    }

    fn validate(&self) -> Result<()> {
        let document = Value::Object(self.document.clone());
        let errors: Vec<String> = schema::validate(&file_schema(), &document)
            .into_iter()
            .map(|error| {
                let key = pointer_key(&error.path);
                let message = format!("{}{}", error.message, allowed_values(&key));
                match self.origin_under(&key) {
                    Some(origin) => format!("{key}: {message} (set by {origin})"),
                    None => format!("{key}: {message}"),
                }
            })
            .collect();
        if !errors.is_empty() {
            bail!("invalid configuration:\n  {}", errors.join("\n  "));
        }
        BuiltinOptions::from_tables(&self.tables())?;
        Settings::from_document(&self.document)?;
        Ok(())
    }

    /// The origin of `key` or, for a table, of the first value inside it.
    fn origin_under(&self, key: &str) -> Option<&Origin> {
        self.origins.get(key).or_else(|| {
            let prefix = format!("{key}.");
            self.origins
                .iter()
                .find(|(k, _)| k.starts_with(&prefix))
                .map(|(_, origin)| origin)
        })
    }

    /// Option tables for built-in and runtime plugins.
    pub(crate) fn tables(&self) -> PluginOptions {
        PluginOptions::from_document(&Value::Object(self.document.clone()))
    }

    pub(crate) fn report(&self) -> Value {
        json!({
            "schema_version": SCHEMA_VERSION,
            "files": self.files,
            "values": self.document,
            "origins": self.origins,
        })
    }
}

/// Global settings that the command line also carries.
#[derive(Debug, Deserialize)]
struct Settings {
    profile: String,
    strict: bool,
    fail_fast: bool,
    include_hidden: bool,
    #[serde(default)]
    filter: Option<String>,
    provenance_page: bool,
    plugins: bool,
    plugin_timeout: u64,
    #[serde(default)]
    plugin_sandbox: Option<String>,
    allow_plugin_kind: Vec<String>,
    deny_plugin_kind: Vec<String>,
    plugin_sandbox_allow_read: Vec<PathBuf>,
    renderer: RendererChoice,
}

#[derive(Debug, Deserialize)]
struct RendererChoice {
    #[serde(rename = "use")]
    name: String,
}

impl Settings {
    fn from_document(document: &Map<String, Value>) -> Result<Self> {
        serde_json::from_value(Value::Object(document.clone())).context("invalid configuration")
    }
}

/// Writes the effective configuration back into the parsed command line, so
/// every command reads one set of values. Flags given on the command line
/// already won during resolution; path flags keep their exact bytes.
pub(crate) fn apply(cli: &mut Cli, matches: &ArgMatches, resolved: &Resolved) -> Result<()> {
    let settings = Settings::from_document(&resolved.document)?;
    let given = |m: &ArgMatches, id: &str| m.value_source(id) == Some(ValueSource::CommandLine);
    cli.no_plugins = !settings.plugins;
    cli.plugin_timeout = settings.plugin_timeout;
    cli.allow_plugin_kind = settings.allow_plugin_kind;
    cli.deny_plugin_kind = settings.deny_plugin_kind;
    cli.plugin_sandbox = settings
        .plugin_sandbox
        .map(|mode| {
            mode.parse::<SandboxMode>()
                .map_err(|e| anyhow!("plugin_sandbox: {e}"))
        })
        .transpose()?;
    if !given(matches, "plugin_sandbox_allow_read") {
        cli.plugin_sandbox_allow_read = settings.plugin_sandbox_allow_read;
    }
    if let (Commands::Convert(args), Some(m)) =
        (&mut cli.command, matches.subcommand_matches("convert"))
    {
        let builtin = BuiltinOptions::from_tables(&resolved.tables())?;
        args.renderer = settings.renderer.name;
        args.profile = settings
            .profile
            .parse()
            .map_err(|e| anyhow!("profile: {e}"))?;
        args.strict = settings.strict;
        args.fail_fast = settings.fail_fast;
        args.include_hidden = settings.include_hidden;
        if !given(m, "filter") {
            args.filter = settings.filter;
        }
        args.no_provenance_page = !settings.provenance_page;
        args.ocr = builtin.ocr.mode;
        args.lang = builtin.ocr.lang;
        args.video_interval = builtin.video.interval;
        args.scene_threshold = builtin.video.scene_threshold;
        args.dedupe_distance = builtin.video.dedupe_distance;
        args.max_video_frames = builtin.video.max_frames;
        args.max_image_frames = builtin.image.max_frames;
        args.no_embedded_subtitles = !builtin.captions.embedded_subtitles;
    }
    Ok(())
}

/// Flags that make a child `anytopdf` process read the same configuration.
pub(crate) fn forward_flags(cli: &Cli) -> Vec<OsString> {
    let mut flags = Vec::new();
    if cli.no_config {
        flags.push("--no-config".into());
    }
    for path in &cli.config_files {
        let mut flag = OsString::from("--config=");
        flag.push(path);
        flags.push(flag);
    }
    for item in &cli.set {
        flags.push(format!("--set={item}").into());
    }
    flags
}

fn config_files(cli: &Cli, env: &Environment) -> Vec<FileStatus> {
    let disabled = env.var(NO_CONFIG_ENV).is_some_and(|v| !v.is_empty());
    if cli.no_config || disabled {
        return Vec::new();
    }
    let explicit: Vec<PathBuf> = if !cli.config_files.is_empty() {
        cli.config_files.clone()
    } else {
        env.var(CONFIG_ENV)
            .map(|paths| {
                std::env::split_paths(paths)
                    .filter(|p| !p.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    if explicit.is_empty() {
        return env
            .user_file
            .iter()
            .map(|path| FileStatus {
                path: path.clone(),
                loaded: false,
                explicit: false,
            })
            .collect();
    }
    explicit
        .into_iter()
        .map(|path| FileStatus {
            path,
            loaded: false,
            explicit: true,
        })
        .collect()
}

fn read_file(path: &Path) -> Result<Map<String, Value>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("cannot read config file {}", path.display()))?;
    let table: toml::Table = toml::from_str(&text)
        .with_context(|| format!("cannot parse config file {}", path.display()))?;
    let value = serde_json::to_value(table)
        .with_context(|| format!("cannot read config file {}", path.display()))?;
    Ok(as_object(value))
}

pub(super) fn as_object(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// Merges `layer` into `target`; tables merge key by key, anything else replaces.
fn merge_into(
    target: &mut Map<String, Value>,
    layer: Map<String, Value>,
    prefix: &str,
    origin: &Origin,
    origins: &mut BTreeMap<String, Origin>,
) {
    for (key, value) in layer {
        // `renderer = "pdf"` is shorthand for `[renderer] use = "pdf"`.
        let value = match value {
            Value::String(name) if prefix.is_empty() && key == "renderer" => json!({ "use": name }),
            value => value,
        };
        // `[enricher.face-detect]` and ANYTOPDF_ENRICHER__FACE_DETECT__… name one table.
        let key = if OPTION_SECTIONS.contains(&prefix) {
            option_table_name(&key)
        } else {
            key
        };
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match (target.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(table)) => {
                merge_into(existing, table, &path, origin, origins);
            }
            (_, Value::Object(table)) => {
                origins.retain(|k, _| k != &path && !k.starts_with(&format!("{path}.")));
                let mut fresh = Map::new();
                merge_into(&mut fresh, table, &path, origin, origins);
                target.insert(key, Value::Object(fresh));
            }
            (_, value) => {
                origins.retain(|k, _| !k.starts_with(&format!("{path}.")));
                origins.insert(path, origin.clone());
                target.insert(key, value);
            }
        }
    }
}
