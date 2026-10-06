//! Layered configuration.
//!
//! Each layer overrides the one before it: built-in defaults, the user config
//! file (or the files named by `--config`), `ANYTOPDF_*` environment variables,
//! command-line flags, and `--set KEY=VALUE`. The merged document has the shape
//! of `schemas/config-file.schema.json`: global keys at the top level and one
//! table per plugin (`[video]`, `[ocr]`, `[whisper]`). Each built-in option has
//! the same three spellings rclone uses: `[video] interval`,
//! `ANYTOPDF_VIDEO_INTERVAL` and `--video-interval` (see `flags.rs`).

use crate::cli::{Cli, Commands};
use anyhow::{Context, Result, anyhow, bail};
use anytopdf_builtin::{BuiltinOptions, option_tables};
use anytopdf_core::{PluginOptions, SandboxMode, option_table_name, schema};
use clap::{ArgMatches, parser::ValueSource};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
mod flags;
mod render;
#[cfg(test)]
mod tests;
mod values;

pub(crate) use flags::augment;
use flags::{builtin_options, builtin_tables};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
};
use values::{allowed_values, file_schema, flag_value, key_path, parse_value, pointer_key};

pub(crate) const SCHEMA_VERSION: &str = "anytopdf.config/1";
pub(crate) const HELP_FOOTER: &str = "\
Every option has three spellings, later layers winning:
  1. built-in defaults
  2. the config file: `interval = 2.0` under `[video]` in ~/.config/anytopdf/config.toml
     (%APPDATA%\\anytopdf\\config.toml on Windows), or in the files named by --config
     or ANYTOPDF_CONFIG instead
  3. the environment: ANYTOPDF_VIDEO_INTERVAL=2 (top-level keys: ANYTOPDF_PROFILE=share)
  4. the flag: anytopdf convert --video-interval 2 (`anytopdf convert --help` lists
     every option, grouped by type)
  5. --set video.interval=2, which also reaches runtime plugin tables

Top-level keys are global settings. [image], [video], [ocr] and [captions] are
the built-in tables; any other table, such as [whisper], is passed to the
runtime plugin of that name in each request (env ANYTOPDF_WHISPER_MODEL works
once the table is in a config file, ANYTOPDF_WHISPER__MODEL always). Start from
`anytopdf config --defaults`. Schema: schemas/config-file.schema.json.";
const FILE_SCHEMA: &str = include_str!("../../../../schemas/config-file.schema.json");
const ENV_PREFIX: &str = "ANYTOPDF_";
/// Path list (platform separator) used instead of the user config file.
pub(crate) const CONFIG_ENV: &str = "ANYTOPDF_CONFIG";
/// Any non-empty value ignores every configuration file.
pub(crate) const NO_CONFIG_ENV: &str = "ANYTOPDF_NO_CONFIG";
/// `ANYTOPDF_*` variables that are secrets or other inputs, never settings.
const RESERVED_ENV: &[&str] = &[
    CONFIG_ENV,
    NO_CONFIG_ENV,
    "ANYTOPDF_FONT",
    "ANYTOPDF_PLUGIN_PATH",
    "ANYTOPDF_QUEUE_TOKEN",
    "ANYTOPDF_REQUIRE_OFFICE",
    "ANYTOPDF_SANDBOX_PROBE",
    "ANYTOPDF_WEBHOOK_SECRET",
];
const RESERVED_ENV_PREFIXES: &[&str] = &["ANYTOPDF_IMAP_", "ANYTOPDF_PRINT_"];

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
    bind("renderer", "--renderer", "renderer"),
    bind("filter", "--filter", "filter"),
    bind("include_hidden", "--include-hidden", "include_hidden"),
    bind("profile", "--profile", "profile"),
    negate(
        "no_provenance_page",
        "--no-provenance-page",
        "provenance_page",
    ),
    negate(
        "no_embedded_subtitles",
        "--no-embedded-subtitles",
        "captions.embedded_subtitles",
    ),
    negate("no_entities", "--no-entities", "entities.enabled"),
    bind("colors", "--colors", "colors.enabled"),
];

/// Every default the schema declares, plus the built-in tables' struct defaults.
fn defaults_document() -> Map<String, Value> {
    let schema = file_schema();
    let mut defaults = Map::new();
    for (key, node) in schema["properties"].as_object().into_iter().flatten() {
        if let Some(default) = node.get("default") {
            defaults.insert(key.clone(), default.clone());
        }
    }
    for table in option_tables() {
        defaults.insert(table.name.to_string(), table.defaults);
    }
    defaults
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
        resolved.merge(defaults_document(), &Origin::Default);
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

    /// Layers the flags of a nested `convert` command line, such as the one
    /// `capture screen` builds for its recording, over this configuration and
    /// writes the result into `cli`.
    pub(crate) fn with_convert(&self, cli: &mut Cli, matches: &ArgMatches) -> Result<Self> {
        let mut resolved = self.clone();
        resolved.apply_flags(matches);
        resolved.validate()?;
        apply(cli, matches, &resolved)?;
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
        let mut tables = builtin_tables();
        tables.extend(
            self.document
                .iter()
                .filter(|(_, v)| v.is_object())
                .map(|(k, _)| k.clone()),
        );
        for (name, value) in &env.vars {
            let Some(name) = name.to_str() else { continue };
            let Some(path) = env_path(name, &tables) else {
                continue;
            };
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
        if let Some(convert) = matches.subcommand_matches("convert") {
            for option in builtin_options() {
                let Some(raw) = option.given(convert) else {
                    continue;
                };
                let path = vec![option.table.clone(), option.key.clone()];
                let value = flag_value(&path, vec![raw]);
                self.set(
                    &path,
                    value,
                    &Origin::Flag {
                        flag: format!("--{}", option.flag()),
                    },
                );
            }
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
            "values": redacted(&self.document),
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
    renderer: String,
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
        args.renderer = settings.renderer;
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
        args.no_embedded_subtitles = !builtin.captions.embedded_subtitles;
        args.no_entities = !builtin.entities;
        args.colors = builtin.colors;
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
        // `[face-detect]` and ANYTOPDF_FACE_DETECT_… name one table.
        let key = if prefix.is_empty() && value.is_object() {
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

/// The configuration key an `ANYTOPDF_*` variable sets, if it is a setting:
/// `ANYTOPDF_PROFILE` is the global `profile`, `ANYTOPDF_VIDEO_INTERVAL` is
/// `interval` in the known table `video` (longest table name wins), and
/// `ANYTOPDF_WHISPER__MODEL` spells the table boundary out for any table.
fn env_path(name: &str, tables: &[String]) -> Option<Vec<String>> {
    if RESERVED_ENV.contains(&name) || RESERVED_ENV_PREFIXES.iter().any(|p| name.starts_with(p)) {
        return None;
    }
    let rest = name.strip_prefix(ENV_PREFIX)?.to_ascii_lowercase();
    if let Some((table, key)) = rest.split_once("__") {
        let path: Vec<String> = std::iter::once(table)
            .chain(key.split("__"))
            .map(str::to_string)
            .collect();
        return path.iter().all(|s| !s.is_empty()).then_some(path);
    }
    let schema = file_schema();
    let is_global = schema["properties"]
        .get(&rest)
        .is_some_and(|node| node.get("$ref").is_none());
    if is_global {
        return Some(vec![rest]);
    }
    tables
        .iter()
        .filter_map(|table| {
            let key = rest.strip_prefix(&format!("{table}_"))?;
            (!key.is_empty()).then(|| vec![table.clone(), key.to_string()])
        })
        .max_by_key(|path| path[0].len())
}

/// Shown in place of values whose key names a credential.
pub(super) const REDACTED: &str = "<redacted>";

/// Whether a key such as `llm_api_key` or `token` holds a credential that
/// `anytopdf config` must not print.
pub(super) fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["key", "token", "secret", "password", "passwd", "credential"]
        .iter()
        .any(|word| key.split(['_', '-']).any(|part| part == *word))
}

fn redacted(map: &Map<String, Value>) -> Map<String, Value> {
    map.iter()
        .map(|(key, value)| {
            let shown = match value {
                _ if is_secret_key(key) => Value::String(REDACTED.into()),
                Value::Object(inner) => Value::Object(redacted(inner)),
                other => other.clone(),
            };
            (key.clone(), shown)
        })
        .collect()
}
