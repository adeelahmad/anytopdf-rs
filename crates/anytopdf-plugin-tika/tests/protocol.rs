use anytopdf_core::{DocumentGraph, RuntimePluginManifest, RuntimeResponse};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    thread,
};

const SOURCE: &str = "11111111-1111-4111-8111-111111111111";

/// One recorded request: method, path, headers (lowercase names) and body.
type Recorded = (String, String, Vec<(String, String)>, Vec<u8>);

/// A minimal HTTP/1.1 server standing in for a Tika server.
struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl Mock {
    fn start(status: u16, reply: Value) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve(stream, status, &reply, &log);
            }
        });
        Mock { url, requests }
    }

    fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(stream: TcpStream, status: u16, reply: &Value, log: &Mutex<Vec<Recorded>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = Vec::new();
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let (name, value) = (name.trim().to_ascii_lowercase(), value.trim().to_string());
            if name == "content-length" {
                length = value.parse().unwrap();
            }
            headers.push((name, value));
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    log.lock().unwrap().push((method, path, headers, body));
    let reply = reply.to_string();
    let mut stream = stream;
    write!(
        stream,
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    )
    .unwrap();
}

fn plugin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf-plugin-tika"));
    for name in [
        "ANYTOPDF_TIKA_URL",
        "ANYTOPDF_TIKA_JAR",
        "ANYTOPDF_TIKA_JAVA",
        "ANYTOPDF_TIKA_TIMEOUT",
    ] {
        command.env_remove(name);
    }
    command
}

fn import(dir: &Path, source: &Path, options: Value, env: &[(&str, &str)]) -> RuntimeResponse {
    let request = dir.join("request.json");
    let response = dir.join("response.json");
    let mut body = json!({
        "protocol": 1,
        "operation": "import",
        "workspace": dir,
        "source": {"id": SOURCE, "path": source, "detected_type": null, "metadata": {"source.filename": "book.epub"}},
    });
    if !options.is_null() {
        body["options"] = options;
    }
    fs::write(&request, body.to_string()).unwrap();
    let mut command = plugin();
    command.envs(env.iter().copied());
    let status = command
        .arg("--anytopdf-request")
        .arg(&request)
        .arg("--anytopdf-response")
        .arg(&response)
        .status()
        .unwrap();
    assert!(status.success());
    serde_json::from_slice(&fs::read(&response).unwrap()).unwrap()
}

fn rmeta() -> Value {
    json!([
        {
            "Content-Type": "application/epub+zip",
            "dc:title": "A Book",
            "X-TIKA:content": "Chapter one.\n\n\n\nThe end."
        },
        {
            "Content-Type": "application/xhtml+xml",
            "X-TIKA:embedded_resource_path": "/OEBPS/appendix.xhtml",
            "X-TIKA:content": "Appendix"
        }
    ])
}

#[test]
fn unconfigured_manifest_is_idle_and_parses() {
    let out = plugin().arg("--anytopdf-manifest").output().unwrap();
    assert!(out.status.success());
    let manifest: RuntimePluginManifest = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(manifest.name, "tika");
    assert!(!manifest.ready);
    let capability = &manifest.capabilities[0];
    assert_eq!(capability.kind, "importer");
    assert_eq!(capability.mime_types, vec!["*/*"]);
    assert!(capability.extensions.iter().any(|x| x == "epub"));
}

#[test]
fn configured_manifest_is_ready() {
    let out = plugin()
        .env("ANYTOPDF_TIKA_URL", "http://127.0.0.1:9")
        .arg("--anytopdf-manifest")
        .output()
        .unwrap();
    let manifest: RuntimePluginManifest = serde_json::from_slice(&out.stdout).unwrap();
    assert!(manifest.ready);
    assert!(manifest.detail.unwrap().contains("127.0.0.1:9"));
}

#[test]
fn imports_through_a_tika_server() {
    let mock = Mock::start(200, rmeta());
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("book.epub");
    fs::write(&source, b"PK fake epub bytes").unwrap();

    let response = import(
        dir.path(),
        &source,
        Value::Null,
        &[("ANYTOPDF_TIKA_URL", &mock.url)],
    );
    assert!(response.ok, "{:?}", response.error);
    assert!(response.warnings.is_empty(), "{:?}", response.warnings);
    assert_eq!(response.units.len(), 2);
    assert_eq!(
        response.units[0].visible_text.as_deref(),
        Some("Chapter one.\n\nThe end.")
    );
    assert_eq!(
        response.units[1].metadata["container.member"],
        "OEBPS/appendix.xhtml"
    );
    let updated = response.source.unwrap();
    assert_eq!(updated.metadata["tika.dc:title"], "A Book");
    assert_eq!(updated.metadata["source.filename"], "book.epub", "kept");
    assert_eq!(
        updated.detected_type.as_deref(),
        Some("application/epub+zip")
    );

    // The host's own validation accepts what the plugin returned.
    let mut units = response.units;
    for unit in &mut units {
        unit.source_id = updated.id;
    }
    DocumentGraph {
        sources: vec![updated],
        units,
        ..Default::default()
    }
    .validate()
    .unwrap();

    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    let (method, path, headers, body) = &requests[0];
    assert_eq!((method.as_str(), path.as_str()), ("PUT", "/rmeta/text"));
    assert_eq!(body, b"PK fake epub bytes");
    assert!(
        headers
            .iter()
            .any(|(n, v)| n == "content-disposition" && v == "attachment; filename=\"book.epub\"")
    );
}

#[test]
fn request_options_override_the_environment() {
    let mock = Mock::start(200, rmeta());
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("book.epub");
    fs::write(&source, b"x").unwrap();
    let response = import(
        dir.path(),
        &source,
        json!({"url": format!("{}/rmeta/text", mock.url)}),
        &[("ANYTOPDF_TIKA_URL", "http://127.0.0.1:9")],
    );
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(mock.requests().len(), 1);
}

#[test]
fn server_errors_and_empty_documents_fail_the_import() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("blob.bin");
    fs::write(&source, b"\0\0").unwrap();

    let failing = Mock::start(422, json!({"error": "unprocessable"}));
    let response = import(
        dir.path(),
        &source,
        Value::Null,
        &[("ANYTOPDF_TIKA_URL", &failing.url)],
    );
    assert!(!response.ok);
    assert!(response.error.unwrap().contains("HTTP 422"));

    let empty = Mock::start(200, json!([{"Content-Type": "application/octet-stream"}]));
    let response = import(
        dir.path(),
        &source,
        Value::Null,
        &[("ANYTOPDF_TIKA_URL", &empty.url)],
    );
    assert!(!response.ok);
    assert_eq!(
        response.error.as_deref(),
        Some("Tika found no text in blob.bin (application/octet-stream)")
    );

    let response = import(dir.path(), &source, Value::Null, &[]);
    assert!(!response.ok);
    assert!(
        response
            .error
            .unwrap()
            .contains("no Tika engine configured")
    );
}

/// A fake `java` that prints recursive JSON for `-jar tika-app.jar ... FILE`.
#[cfg(unix)]
#[test]
fn falls_back_from_an_unreachable_server_to_the_jar() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    let jar = tools.path().join("tika-app.jar");
    fs::write(&jar, b"jar").unwrap();
    let staged = tools.path().join("java.sh");
    fs::write(
        &staged,
        "#!/bin/sh\n[ \"$2\" = -jar ] && [ \"$4\" = --jsonRecursive ] || exit 9\n\
         printf '%s' '[{\"Content-Type\":\"application/vnd.ms-outlook\",\"X-TIKA:content\":\"Hello from '\"$(basename \"$7\")\"'\"}]'\n",
    )
    .unwrap();
    // Copy rather than write the executable, so no concurrently forked test
    // inherits a writable descriptor on it (ETXTBSY).
    let java = tools.path().join("java");
    assert!(
        Command::new("cp")
            .arg(&staged)
            .arg(&java)
            .status()
            .unwrap()
            .success()
    );
    fs::set_permissions(&java, fs::Permissions::from_mode(0o755)).unwrap();

    let source = dir.path().join("mail.msg");
    fs::write(&source, b"msg").unwrap();
    let response = import(
        dir.path(),
        &source,
        Value::Null,
        &[
            ("ANYTOPDF_TIKA_URL", "http://127.0.0.1:9"),
            ("ANYTOPDF_TIKA_JAR", jar.to_str().unwrap()),
            ("ANYTOPDF_TIKA_JAVA", java.to_str().unwrap()),
            ("ANYTOPDF_TIKA_TIMEOUT", "5"),
        ],
    );
    assert!(response.ok, "{:?}", response.error);
    assert_eq!(
        response.units[0].visible_text.as_deref(),
        Some("Hello from mail.msg")
    );
    assert_eq!(response.warnings.len(), 1, "{:?}", response.warnings);
    assert!(response.warnings[0].contains("trying the next engine"));
    let leftovers: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("tika-"))
        .collect();
    assert!(leftovers.is_empty(), "temporary output removed");
}
