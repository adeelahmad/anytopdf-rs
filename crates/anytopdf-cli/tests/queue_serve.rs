use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::Arc;

#[path = "common/process.rs"]
mod process;

use process::command;

const TOKEN: &str = "test-token-0123456789";

struct Server {
    child: Child,
    base: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(dir: &Path, extra: &[&str]) -> Server {
    let mut child = command()
        .args(["queue", "serve", "q", "--listen", "127.0.0.1:0"])
        .args(extra)
        .env("ANYTOPDF_QUEUE_TOKEN", TOKEN)
        .current_dir(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start queue serve");
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut line = String::new();
    stderr.read_line(&mut line).unwrap();
    // Keep draining the server log so its writes never hit a closed pipe.
    std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
    let url = line
        .trim()
        .strip_prefix("queue serve: listening on ")
        .unwrap_or_else(|| panic!("unexpected first line: {line:?}"))
        .trim_end_matches("/v1/jobs")
        .to_string();
    Server { child, base: url }
}

/// An HTTP client; with `trust_fixture_ca` it trusts the print crate's test CA.
fn agent(trust_fixture_ca: bool) -> ureq::Agent {
    let mut builder = ureq::Agent::config_builder().http_status_as_error(false);
    if trust_fixture_ca {
        let ca = fixtures().join("ca.pem");
        let certs = ureq::tls::parse_pem(&fs::read(ca).unwrap())
            .filter_map(Result::ok)
            .filter_map(|item| match item {
                ureq::tls::PemItem::Certificate(c) => Some(c),
                _ => None,
            })
            .collect::<Vec<_>>();
        builder = builder.tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::Specific(Arc::new(certs)))
                .build(),
        );
    }
    builder.build().into()
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../anytopdf-print/tests/fixtures")
}

fn body_json(response: ureq::http::Response<ureq::Body>) -> (u16, Value) {
    let status = response.status().as_u16();
    let text = response.into_body().read_to_string().unwrap();
    (status, serde_json::from_str(&text).unwrap())
}

fn upload(agent: &ureq::Agent, url: &str, token: &str, bytes: &[u8]) -> (u16, Value) {
    body_json(
        agent
            .post(url)
            .header("authorization", &format!("Bearer {token}"))
            .send(bytes)
            .unwrap(),
    )
}

#[test]
fn uploads_become_jobs_whose_status_and_pdf_can_be_fetched() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let server = start(dir, &["--", "--ocr", "off"]);
    let http = agent(false);
    let jobs = format!("{}/v1/jobs", server.base);

    let (code, _) = upload(&http, &jobs, "wrong-token-0123456789", b"x");
    assert_eq!(code, 401);
    let missing = http.post(&jobs).send(&b"x"[..]).unwrap();
    assert_eq!(missing.status().as_u16(), 401);

    let url = format!("{jobs}?filename=..%2F..%2Fnotes.txt");
    let (code, job) = upload(&http, &url, TOKEN, b"uploaded over http\n");
    assert_eq!(code, 202, "{job}");
    assert_eq!(job["state"], "queued");
    assert_eq!(job["origin"], "http");
    assert_eq!(job["inputs"][0], "notes.txt");
    let id = job["job_id"].as_str().unwrap().to_string();
    let record: Value = serde_json::from_slice(
        &fs::read(dir.join("q/jobs/pending").join(format!("{id}.json"))).unwrap(),
    )
    .unwrap();
    let input = PathBuf::from(record["inputs"][0].as_str().unwrap());
    // Compare the tail only: the server sees the canonical cwd (/private/var on macOS).
    let tail: PathBuf = ["q", "work", id.as_str(), "input", "notes.txt"]
        .iter()
        .collect();
    assert!(input.ends_with(&tail), "{}", input.display());
    assert_eq!(record["convert_args"], serde_json::json!(["--ocr", "off"]));

    let get = |path: &str| {
        http.get(&format!("{jobs}/{path}"))
            .header("authorization", &format!("Bearer {TOKEN}"))
            .call()
            .unwrap()
    };
    assert_eq!(get(&format!("{id}/output")).status().as_u16(), 409);
    assert_eq!(
        get(&format!("job_{}", "0".repeat(32))).status().as_u16(),
        404
    );
    assert_eq!(get("../../etc").status().as_u16(), 404);

    let worker = command()
        .args(["queue", "work", "q", "--once", "--poll-interval", "0.1"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        worker.status.success(),
        "{}",
        String::from_utf8_lossy(&worker.stderr)
    );

    let (code, done) = body_json(get(&id));
    assert_eq!(code, 200);
    assert_eq!(done["state"], "succeeded");
    assert_eq!(done["output"], "outbox/notes.pdf");
    let pdf = get(&format!("{id}/output"));
    assert_eq!(pdf.status().as_u16(), 200);
    assert_eq!(pdf.headers()["content-type"], "application/pdf");
    let bytes = pdf.into_body().read_to_vec().unwrap();
    assert!(bytes.starts_with(b"%PDF-"));
    assert_eq!(bytes, fs::read(dir.join("q/outbox/notes.pdf")).unwrap());
}

/// Send a raw request head (and optional body) and return the status code.
fn raw(base: &str, head: &str, body: &[u8]) -> u16 {
    let addr = base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(head.as_bytes()).unwrap();
    let _ = stream.write_all(body);
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    response
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("no status in {response:?}"))
}

#[test]
fn oversized_chunked_and_short_uploads_are_refused_without_a_job() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let server = start(dir, &["--max-upload-mb", "1"]);
    let auth = format!("Authorization: Bearer {TOKEN}\r\n");
    let big = format!(
        "POST /v1/jobs HTTP/1.1\r\nHost: x\r\n{auth}Content-Length: {}\r\n\r\n",
        2 * 1024 * 1024
    );
    assert_eq!(raw(&server.base, &big, b""), 413);
    let chunked =
        format!("POST /v1/jobs HTTP/1.1\r\nHost: x\r\n{auth}Transfer-Encoding: chunked\r\n\r\n");
    assert_eq!(raw(&server.base, &chunked, b"0\r\n\r\n"), 411);
    let no_length = format!("POST /v1/jobs HTTP/1.1\r\nHost: x\r\n{auth}\r\n");
    assert_eq!(raw(&server.base, &no_length, b""), 411);
    let short = format!("POST /v1/jobs HTTP/1.1\r\nHost: x\r\n{auth}Content-Length: 10\r\n\r\n");
    let addr = server.base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(short.as_bytes()).unwrap();
    stream.write_all(b"abc").unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut response = String::new();
    let _ = stream.read_to_string(&mut response);
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
    let wrong_method = format!("DELETE /v1/jobs HTTP/1.1\r\nHost: x\r\n{auth}\r\n");
    assert_eq!(raw(&server.base, &wrong_method, b""), 405);

    let pending = fs::read_dir(dir.join("q/jobs/pending")).unwrap().count();
    assert_eq!(pending, 0);
    assert_eq!(fs::read_dir(dir.join("q/work")).unwrap().count(), 0);
}

#[test]
fn uploads_work_over_tls() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let cert = fixtures().join("server.pem");
    let key = fixtures().join("server.key");
    let server = start(
        dir,
        &[
            "--tls-cert",
            cert.to_str().unwrap(),
            "--tls-key",
            key.to_str().unwrap(),
        ],
    );
    assert!(
        server.base.starts_with("https://127.0.0.1:"),
        "{}",
        server.base
    );
    let url = server.base.replace("127.0.0.1", "localhost") + "/v1/jobs?filename=a.txt";
    let (code, job) = upload(&agent(true), &url, TOKEN, b"over tls\n");
    assert_eq!(code, 202, "{job}");
    assert_eq!(fs::read_dir(dir.join("q/jobs/pending")).unwrap().count(), 1);
}

#[test]
fn unsafe_listeners_and_missing_tokens_are_usage_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let cert = fixtures().join("server.pem");
    let key = fixtures().join("server.key");
    let tls = [
        "--tls-cert",
        cert.to_str().unwrap(),
        "--tls-key",
        key.to_str().unwrap(),
    ];
    let run = |args: &[&str], token: Option<&str>| {
        let mut cmd = command();
        cmd.args(["queue", "serve", "q"])
            .args(args)
            .current_dir(dir);
        match token {
            Some(t) => cmd.env("ANYTOPDF_QUEUE_TOKEN", t),
            None => cmd.env_remove("ANYTOPDF_QUEUE_TOKEN"),
        };
        cmd.output().unwrap().status.code()
    };
    assert_eq!(run(&["--listen", "192.0.2.1:8640"], Some(TOKEN)), Some(2));
    let mut public = vec!["--listen", "0.0.0.0:0"];
    public.extend(tls);
    assert_eq!(run(&public, Some(TOKEN)), Some(2));
    assert_eq!(run(&["--listen", "127.0.0.1:0"], None), Some(2));
    assert_eq!(run(&["--listen", "127.0.0.1:0"], Some("short")), Some(2));
    assert_eq!(
        run(
            &["--listen", "127.0.0.1:0", "--", "-o", "x.pdf"],
            Some(TOKEN)
        ),
        Some(2)
    );
    assert_eq!(
        run(&["--tls-cert", cert.to_str().unwrap()], Some(TOKEN)),
        Some(2)
    );
}
