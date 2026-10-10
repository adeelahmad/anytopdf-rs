//! Settings and the two ways of reaching Tika: a Tika server's `/rmeta/text`
//! endpoint, or `java -jar tika-app.jar --jsonRecursive --text`. Both answer
//! with the same recursive metadata JSON.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Below the host's default 60-second plugin timeout, so a slow parse is
/// reported by the plugin instead of the call being killed.
const DEFAULT_TIMEOUT: u64 = 50;
/// Largest Tika answer read; the host caps the plugin's own response at
/// 16 MiB and `rmeta` trims the text to fit.
const MAX_ANSWER_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum Engine {
    Server(String),
    App { java: PathBuf, jar: PathBuf },
}

#[derive(Debug, Clone)]
pub struct Config {
    pub engines: Vec<Engine>,
    pub timeout: Duration,
}

impl Config {
    /// Reads `ANYTOPDF_TIKA_{URL,JAR,JAVA,TIMEOUT}`; `None` when neither a
    /// server nor a jar is set.
    pub fn from_settings(var: impl Fn(&str) -> Option<String>) -> Result<Option<Self>> {
        let get = |name: &str| {
            var(&format!("ANYTOPDF_TIKA_{name}"))
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let timeout = match get("TIMEOUT") {
            Some(v) => v.parse::<u64>().ok().filter(|s| *s > 0).with_context(|| {
                format!("ANYTOPDF_TIKA_TIMEOUT must be whole seconds, got {v:?}")
            })?,
            None => DEFAULT_TIMEOUT,
        };
        let mut engines = Vec::new();
        if let Some(url) = get("URL") {
            if !url.starts_with("http://") && !url.starts_with("https://") {
                bail!("ANYTOPDF_TIKA_URL must be an http:// or https:// URL, got {url:?}");
            }
            engines.push(Engine::Server(url.trim_end_matches('/').to_string()));
        }
        if let Some(jar) = get("JAR") {
            let java = get("JAVA").map(PathBuf::from).unwrap_or_else(|| {
                let exe = if cfg!(windows) { "java.exe" } else { "java" };
                var("JAVA_HOME")
                    .filter(|h| !h.trim().is_empty())
                    .map(|home| Path::new(&home).join("bin").join(exe))
                    .filter(|p| p.is_file())
                    .unwrap_or_else(|| PathBuf::from("java"))
            });
            engines.push(Engine::App {
                java,
                jar: PathBuf::from(jar),
            });
        }
        if engines.is_empty() {
            return Ok(None);
        }
        Ok(Some(Config {
            engines,
            timeout: Duration::from_secs(timeout),
        }))
    }

    pub fn describe(&self) -> String {
        let engines: Vec<String> = self
            .engines
            .iter()
            .map(|e| match e {
                Engine::Server(url) => format!("Apache Tika server at {url}"),
                Engine::App { jar, .. } => format!("tika-app jar {}", jar.display()),
            })
            .collect();
        engines.join(", then ")
    }
}

/// The endpoint for a configured server URL, which may already name it.
pub fn endpoint(url: &str) -> String {
    if url.ends_with("/rmeta/text") {
        url.to_string()
    } else {
        format!("{url}/rmeta/text")
    }
}

pub fn server(url: &str, path: &Path, timeout: Duration) -> Result<Vec<Value>> {
    let endpoint = endpoint(url);
    let mut config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .user_agent(concat!("anytopdf-plugin-tika/", env!("CARGO_PKG_VERSION")));
    // A local Tika server must not be reached through an outbound proxy.
    if is_loopback(url) {
        config = config.proxy(None);
    }
    let agent: ureq::Agent = config.build().into();
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().replace(['"', '\\', '\r', '\n'], "_"))
        .unwrap_or_default();
    let mut response = agent
        .put(&endpoint)
        .header("Accept", "application/json")
        // Tika uses the name as a detection hint, as it does for its own CLI.
        .header(
            "Content-Disposition",
            &format!("attachment; filename=\"{name}\""),
        )
        .send(file)
        .with_context(|| format!("request {endpoint}"))?;
    let status = response.status().as_u16();
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER_BYTES)
        .read_to_string()
        .context("read Tika server answer")?;
    if !(200..300).contains(&status) {
        let detail: String = body.chars().take(200).collect();
        bail!("Tika server answered HTTP {status}: {}", detail.trim());
    }
    parse(&body)
}

fn is_loopback(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

pub fn app(
    java: &Path,
    jar: &Path,
    path: &Path,
    workspace: &Path,
    timeout: Duration,
) -> Result<Vec<Value>> {
    if !jar.is_file() {
        bail!("tika-app jar {} does not exist", jar.display());
    }
    // Output goes to files in the job workspace, so a large answer never
    // fills a pipe while this process waits for Java to exit.
    let token = std::process::id();
    let out_path = workspace.join(format!("tika-{token}.json"));
    let err_path = workspace.join(format!("tika-{token}.log"));
    let mut child = Command::new(java)
        .arg("-Djava.awt.headless=true")
        .arg("-jar")
        .arg(jar)
        .args(["--jsonRecursive", "--text", "--encoding=UTF-8"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&out_path).context("create Tika output file")?)
        .stderr(fs::File::create(&err_path).context("create Tika log file")?)
        .spawn()
        .with_context(|| format!("run {} (is Java installed?)", java.display()))?;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&out_path);
            bail!("tika-app took longer than {}s", timeout.as_secs());
        }
        thread::sleep(Duration::from_millis(50));
    };
    let log = fs::read_to_string(&err_path).unwrap_or_default();
    let _ = fs::remove_file(&err_path);
    let output = fs::metadata(&out_path)
        .ok()
        .filter(|m| m.len() <= MAX_ANSWER_BYTES)
        .map(|_| fs::read(&out_path));
    let _ = fs::remove_file(&out_path);
    if !status.success() {
        let detail: String = log
            .trim()
            .lines()
            .last()
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect();
        bail!("tika-app exited with {status}: {detail}");
    }
    let bytes = output
        .context("tika-app output exceeds the size limit")?
        .context("read tika-app output")?;
    parse(&String::from_utf8_lossy(&bytes))
}

/// Tika's recursive metadata answer: an array of objects, the file first.
pub fn parse(body: &str) -> Result<Vec<Value>> {
    let value: Value = serde_json::from_str(body.trim()).context("Tika answer is not JSON")?;
    match value {
        Value::Array(items) if items.iter().all(Value::is_object) => Ok(items),
        Value::Object(_) => Ok(vec![value]),
        _ => bail!("Tika answer is not a list of documents"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn settings(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn nothing_configured_is_idle() {
        assert!(Config::from_settings(settings(&[])).unwrap().is_none());
        let blank = settings(&[("ANYTOPDF_TIKA_URL", "  ")]);
        assert!(Config::from_settings(blank).unwrap().is_none());
    }

    #[test]
    fn server_comes_before_the_jar() {
        let config = Config::from_settings(settings(&[
            ("ANYTOPDF_TIKA_JAR", "/opt/tika-app.jar"),
            ("ANYTOPDF_TIKA_JAVA", "/opt/jdk/bin/java"),
            ("ANYTOPDF_TIKA_URL", "http://localhost:9998/"),
            ("ANYTOPDF_TIKA_TIMEOUT", "5"),
        ]))
        .unwrap()
        .unwrap();
        assert_eq!(
            config.engines,
            vec![
                Engine::Server("http://localhost:9998".into()),
                Engine::App {
                    java: "/opt/jdk/bin/java".into(),
                    jar: "/opt/tika-app.jar".into()
                }
            ]
        );
        assert_eq!(config.timeout, Duration::from_secs(5));
        assert_eq!(
            config.describe(),
            "Apache Tika server at http://localhost:9998, then tika-app jar /opt/tika-app.jar"
        );
    }

    #[test]
    fn jar_without_java_setting_runs_java_from_path() {
        let config = Config::from_settings(settings(&[("ANYTOPDF_TIKA_JAR", "tika.jar")]))
            .unwrap()
            .unwrap();
        assert_eq!(
            config.engines,
            vec![Engine::App {
                java: "java".into(),
                jar: "tika.jar".into()
            }]
        );
        assert_eq!(config.timeout, Duration::from_secs(DEFAULT_TIMEOUT));
    }

    #[test]
    fn invalid_settings_are_errors() {
        let url = Config::from_settings(settings(&[("ANYTOPDF_TIKA_URL", "localhost:9998")]));
        assert!(url.unwrap_err().to_string().contains("http://"));
        let timeout = Config::from_settings(settings(&[
            ("ANYTOPDF_TIKA_URL", "http://tika"),
            ("ANYTOPDF_TIKA_TIMEOUT", "soon"),
        ]));
        assert!(timeout.unwrap_err().to_string().contains("whole seconds"));
    }

    #[test]
    fn endpoint_accepts_a_base_url_or_the_full_endpoint() {
        assert_eq!(endpoint("http://t:9998"), "http://t:9998/rmeta/text");
        assert_eq!(endpoint("http://t/rmeta/text"), "http://t/rmeta/text");
    }

    #[test]
    fn loopback_servers_skip_the_proxy() {
        assert!(is_loopback("http://localhost:9998"));
        assert!(is_loopback("http://127.0.0.1:9998/rmeta/text"));
        assert!(is_loopback("http://[::1]:9998"));
        assert!(!is_loopback("http://tika.internal:9998"));
    }

    #[test]
    fn parse_accepts_a_list_or_a_single_document() {
        assert_eq!(parse(r#"[{"a":"1"},{"b":"2"}]"#).unwrap().len(), 2);
        assert_eq!(parse(r#"{"a":"1"}"#).unwrap().len(), 1);
        assert!(parse("[1]").is_err());
        assert!(parse("<html>").is_err());
    }

    #[test]
    fn missing_jar_is_reported_before_running_java() {
        let dir = tempfile::tempdir().unwrap();
        let err = app(
            Path::new("java"),
            &dir.path().join("missing.jar"),
            &dir.path().join("in.epub"),
            dir.path(),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"));
    }
}
