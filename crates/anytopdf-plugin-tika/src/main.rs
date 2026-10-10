//! `anytopdf-plugin-tika`: an anytopdf runtime plugin (protocol v1) that
//! imports whatever Apache Tika can read (EPUB, Outlook `.msg`, XPS, iWork,
//! Visio and the rest of Tika's thousand-odd formats) as text units.
//!
//! It registers as a catch-all importer (`*/*`), so it only sees a file no
//! built-in importer claims, plus a short list of extensions that text
//! sniffing would otherwise mangle. It talks to a Tika server
//! (`ANYTOPDF_TIKA_URL`, e.g. `docker run -p 9998:9998 apache/tika`) or runs a
//! `tika-app` jar with Java (`ANYTOPDF_TIKA_JAR`), and stays idle until one of
//! them is set.

mod config;
mod rmeta;

use anyhow::{Context, Result, bail};
use config::{Config, Engine};
use serde_json::{Value, json};
use std::{env, fs, path::Path, process::ExitCode};

const PROTOCOL: u64 = 1;

/// Extensions claimed ahead of text sniffing. Built-in importers still win
/// any tie through priority; everything else reaches Tika through `*/*`.
const EXTENSIONS: &[&str] = &[
    "epub", "fb2", "msg", "xps", "oxps", "chm", "pages", "numbers", "key", "vsd", "vsdx", "pub",
    "mpp", "sxw", "sxc", "sxi", "wps", "one",
];

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--anytopdf-manifest" => {
            println!("{}", manifest(Config::from_settings(|n| env::var(n).ok())));
            ExitCode::SUCCESS
        }
        [flag] if flag == "--version" => {
            println!("anytopdf-plugin-tika {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [req_flag, request, resp_flag, response]
            if req_flag == "--anytopdf-request" && resp_flag == "--anytopdf-response" =>
        {
            let body = handle(Path::new(request), |n| env::var(n).ok()).unwrap_or_else(|e| {
                json!({"protocol": PROTOCOL, "ok": false, "warnings": [], "error": format!("{e:#}")})
            });
            match write_atomic(Path::new(response), &body) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("anytopdf-plugin-tika: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: anytopdf-plugin-tika --anytopdf-manifest\n       \
                 anytopdf-plugin-tika --anytopdf-request REQUEST --anytopdf-response RESPONSE"
            );
            ExitCode::from(2)
        }
    }
}

/// Reports `ready: false` until a server or jar is configured, so a copy
/// bundled with anytopdf stays idle and never claims a file. A
/// misconfiguration is ready so its error surfaces.
fn manifest(config: Result<Option<Config>>) -> Value {
    let (ready, detail) = match &config {
        Ok(None) => (
            false,
            Some(
                "set ANYTOPDF_TIKA_URL to an Apache Tika server (for example http://localhost:9998) \
                 or ANYTOPDF_TIKA_JAR to a tika-app jar"
                    .to_string(),
            ),
        ),
        Ok(Some(config)) => (true, Some(config.describe())),
        Err(_) => (true, None),
    };
    json!({
        "protocol": PROTOCOL,
        "name": "tika",
        "version": env!("CARGO_PKG_VERSION"),
        "ready": ready,
        "detail": detail,
        "capabilities": [{
            "kind": "importer",
            "extensions": EXTENSIONS,
            "mime_types": ["*/*"],
            "priority": -100
        }]
    })
}

fn handle(request_path: &Path, var: impl Fn(&str) -> Option<String>) -> Result<Value> {
    let raw = fs::read_to_string(request_path).context("read request")?;
    let request: Value = serde_json::from_str(&raw).context("parse request")?;
    if request["protocol"].as_u64() != Some(PROTOCOL) {
        bail!("unsupported protocol {}", request["protocol"]);
    }
    if request["operation"] != "import" {
        bail!("unsupported operation {}", request["operation"]);
    }
    let source = &request["source"];
    let path = source["path"]
        .as_str()
        .context("import request has no source path")?;
    let workspace = request["workspace"]
        .as_str()
        .context("import request has no workspace")?;
    // The layered config (`[tika]` table, `--set tika.KEY=...`) arrives as
    // request `options`; environment variables fill in what it leaves out.
    let options = request["options"].clone();
    let setting = |name: &str| {
        let from_options = name
            .strip_prefix("ANYTOPDF_TIKA_")
            .map(str::to_ascii_lowercase)
            .and_then(|k| match &options[k.as_str()] {
                Value::String(s) => Some(s.clone()),
                Value::Number(n) => Some(n.to_string()),
                _ => None,
            });
        from_options.or_else(|| var(name))
    };
    let config = Config::from_settings(setting)?
        .context("no Tika engine configured: set ANYTOPDF_TIKA_URL or ANYTOPDF_TIKA_JAR")?;

    let mut warnings = Vec::new();
    let documents = extract(
        &config,
        Path::new(path),
        Path::new(workspace),
        &mut warnings,
    )?;
    let name = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let import = rmeta::units(&documents, &name, &mut warnings)?;

    let mut updated = source.clone();
    let metadata = updated
        .as_object_mut()
        .context("import request source is not an object")?
        .entry("metadata")
        .or_insert_with(|| json!({}));
    if let Some(metadata) = metadata.as_object_mut() {
        for (key, value) in import.metadata {
            metadata.insert(key, Value::String(value));
        }
    }
    if updated["detected_type"].is_null()
        && let Some(content_type) = import.content_type
    {
        updated["detected_type"] = Value::String(content_type);
    }
    Ok(json!({
        "protocol": PROTOCOL,
        "ok": true,
        "warnings": warnings,
        "source": updated,
        "units": import.units,
    }))
}

/// Runs the configured engines in order (server first, then the jar) and
/// returns Tika's recursive metadata list: the file itself, then each
/// embedded document.
fn extract(
    config: &Config,
    path: &Path,
    workspace: &Path,
    warnings: &mut Vec<String>,
) -> Result<Vec<Value>> {
    let mut last = None;
    for (i, engine) in config.engines.iter().enumerate() {
        let attempt = match engine {
            Engine::Server(url) => config::server(url, path, config.timeout),
            Engine::App { java, jar } => config::app(java, jar, path, workspace, config.timeout),
        };
        match attempt {
            Ok(documents) => return Ok(documents),
            Err(e) if i + 1 < config.engines.len() => {
                warnings.push(format!("tika: {e:#}; trying the next engine"));
                last = Some(e);
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no Tika engine configured")))
}

fn write_atomic(path: &Path, body: &Value) -> Result<()> {
    let mut name = path
        .file_name()
        .context("response path has no file name")?
        .to_os_string();
    name.push(".tmp");
    let temporary = path.with_file_name(name);
    fs::write(&temporary, serde_json::to_vec(body)?).context("write response")?;
    fs::rename(&temporary, path).context("publish response")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_manifest_is_idle_but_still_valid() {
        let idle = manifest(Ok(None));
        assert_eq!(idle["ready"], false);
        assert!(
            idle["detail"]
                .as_str()
                .unwrap()
                .contains("ANYTOPDF_TIKA_URL")
        );
        assert_eq!(idle["capabilities"][0]["kind"], "importer");
        assert_eq!(idle["capabilities"][0]["mime_types"], json!(["*/*"]));
    }

    #[test]
    fn misconfigured_manifest_is_ready_so_the_error_surfaces() {
        let manifest = manifest(Err(anyhow::anyhow!("bad timeout")));
        assert_eq!(manifest["ready"], true);
    }

    #[test]
    fn configured_manifest_names_its_engine() {
        let config = Config::from_settings(|n| {
            (n == "ANYTOPDF_TIKA_URL").then(|| "http://tika:9998/".to_string())
        });
        let manifest = manifest(config);
        assert_eq!(manifest["ready"], true);
        assert!(
            manifest["detail"]
                .as_str()
                .unwrap()
                .contains("http://tika:9998")
        );
    }
}
