use anytopdf_print::{Allowlist, Front, FrontConfig, Receipts, Users, load_tls};
use base64ct::{Base64, Encoding};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

type Tls = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// One request as the print helper received it.
#[derive(Debug, Clone)]
struct Seen {
    head: String,
    body: Vec<u8>,
}

fn read_head(stream: &mut impl Read, pending: &mut Vec<u8>) -> Option<String> {
    let mut buf = [0u8; 4096];
    loop {
        if let Some(i) = pending.windows(4).position(|w| w == b"\r\n\r\n") {
            let head: Vec<u8> = pending.drain(..i + 4).collect();
            return Some(String::from_utf8_lossy(&head[..i]).into_owned());
        }
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => pending.extend_from_slice(&buf[..n]),
        }
    }
}

fn read_exact_buffered(stream: &mut impl Read, pending: &mut Vec<u8>, n: usize) -> Vec<u8> {
    let mut buf = [0u8; 4096];
    while pending.len() < n {
        let got = stream.read(&mut buf).unwrap();
        assert!(got > 0, "connection closed mid-body");
        pending.extend_from_slice(&buf[..got]);
    }
    pending.drain(..n).collect()
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.split("\r\n").skip(1).find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

fn read_body(stream: &mut impl Read, pending: &mut Vec<u8>, head: &str) -> Vec<u8> {
    if header(head, "transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        let mut body = Vec::new();
        loop {
            let line = read_line(stream, pending);
            let size = usize::from_str_radix(line.split(';').next().unwrap().trim(), 16).unwrap();
            if size == 0 {
                while !read_line(stream, pending).is_empty() {}
                return body;
            }
            body.extend(read_exact_buffered(stream, pending, size));
            read_exact_buffered(stream, pending, 2);
        }
    }
    let len = header(head, "content-length").map_or(0, |v| v.parse().unwrap());
    read_exact_buffered(stream, pending, len)
}

fn read_line(stream: &mut impl Read, pending: &mut Vec<u8>) -> String {
    let mut buf = [0u8; 1];
    while !pending.windows(2).any(|w| w == b"\r\n") {
        assert_eq!(
            stream.read(&mut buf).unwrap(),
            1,
            "connection closed mid-line"
        );
        pending.push(buf[0]);
    }
    let i = pending.windows(2).position(|w| w == b"\r\n").unwrap();
    let line: Vec<u8> = pending.drain(..i + 2).collect();
    String::from_utf8_lossy(&line[..i]).into_owned()
}

/// A stand-in print helper: records each request and answers `hello`,
/// keeping connections alive.
fn fake_helper() -> (SocketAddr, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                let mut pending = Vec::new();
                while let Some(head) = read_head(&mut stream, &mut pending) {
                    let body = read_body(&mut stream, &mut pending, &head);
                    log.lock().unwrap().push(Seen { head, body });
                    let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello");
                }
            });
        }
    });
    (addr, seen)
}

fn start_front(upstream: SocketAddr, allow: &[&str], receipts: Option<&Path>) -> SocketAddr {
    let mut users = Users::new();
    users.set_password("adeel", "correct horse").unwrap();
    let front = Front::bind(FrontConfig {
        listen: "127.0.0.1:0".parse().unwrap(),
        upstream,
        tls: load_tls(&fixture("server.pem"), &fixture("server.key")).unwrap(),
        users: Arc::new(users),
        allow: Allowlist::parse(allow).unwrap(),
        max_connections: 8,
        idle_timeout: Duration::from_secs(5),
        receipts: receipts.map(|p| Receipts::open(p).unwrap()),
    })
    .unwrap();
    let addr = front.local_addr().unwrap();
    std::thread::spawn(move || front.serve());
    addr
}

fn connect(addr: SocketAddr) -> std::io::Result<Tls> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_file_iter(fixture("ca.pem")).unwrap() {
        roots.add(cert.unwrap()).unwrap();
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_root_certificates(roots)
    .with_no_client_auth();
    let conn =
        rustls::ClientConnection::new(Arc::new(config), ServerName::try_from("localhost").unwrap())
            .unwrap();
    let tcp = TcpStream::connect(addr)?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;
    Ok(rustls::StreamOwned::new(conn, tcp))
}

fn auth(user: &str, password: &str) -> String {
    format!(
        "Authorization: Basic {}\r\n",
        Base64::encode_string(format!("{user}:{password}").as_bytes())
    )
}

fn attr(tag: u8, name: &str, value: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
    out
}

/// An IPP request whose client claims to be `claimed`, followed by document bytes.
fn ipp(operation: u16, claimed: &str, document: &[u8]) -> Vec<u8> {
    let mut body = vec![2, 0];
    body.extend_from_slice(&operation.to_be_bytes());
    body.extend_from_slice(&[0, 0, 0, 1, 0x01]);
    body.extend(attr(0x47, "attributes-charset", b"utf-8"));
    body.extend(attr(0x48, "attributes-natural-language", b"en"));
    body.extend(attr(0x42, "requesting-user-name", claimed.as_bytes()));
    body.extend(attr(0x42, "job-name", b"Receipt"));
    body.push(0x03);
    body.extend_from_slice(document);
    body
}

fn post(body: &[u8], extra_headers: &str) -> Vec<u8> {
    let mut out = format!(
        "POST /ipp/print/anytopdf HTTP/1.1\r\nHost: printer.home.example:8631\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n{extra_headers}\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

/// Reads one final response (skipping 100 Continue) and returns status line and body.
fn response(tls: &mut Tls, pending: &mut Vec<u8>) -> Option<(String, Vec<u8>)> {
    loop {
        let head = read_head(tls, pending)?;
        if head.starts_with("HTTP/1.1 100") {
            continue;
        }
        let body = read_body(tls, pending, &head);
        return Some((head, body));
    }
}

fn exchange(addr: SocketAddr, request: &[u8]) -> std::io::Result<String> {
    let mut tls = connect(addr)?;
    tls.write_all(request)?;
    let mut pending = Vec::new();
    match response(&mut tls, &mut pending) {
        Some((head, body)) => Ok(head + "\r\n\r\n" + &String::from_utf8_lossy(&body)),
        None => Err(std::io::Error::other("no response")),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn request_without_credentials_gets_a_basic_challenge_and_never_reaches_the_helper() {
    let (helper, seen) = fake_helper();
    let front = start_front(helper, &[], None);
    let response = exchange(front, &post(&ipp(2, "x", b"DOC"), "")).unwrap();
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    assert!(response.contains("WWW-Authenticate: Basic realm=\"anytopdf\""));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn wrong_password_is_refused() {
    let (helper, seen) = fake_helper();
    let front = start_front(helper, &[], None);
    let request = post(&ipp(2, "x", b"DOC"), &auth("adeel", "wrong"));
    let response = exchange(front, &request).unwrap();
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn print_job_reaches_the_helper_as_the_signed_in_user_with_a_receipt() {
    let (helper, seen) = fake_helper();
    let dir = tempfile::tempdir().unwrap();
    let receipts = dir.path().join("receipts.jsonl");
    let front = start_front(helper, &[], Some(&receipts));
    let request = post(
        &ipp(2, "mallory", b"RaS2-document"),
        &auth("adeel", "correct horse"),
    );
    let response = exchange(front, &request).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.ends_with("hello"), "{response}");

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let Seen { head, body } = &seen[0];
    assert!(head.starts_with("POST /ipp/print/anytopdf HTTP/1.1"));
    assert!(header(head, "authorization").is_none(), "{head}");
    assert_eq!(
        header(head, "host"),
        Some(helper.to_string().as_str()),
        "{head}"
    );
    assert_eq!(head.matches("Host:").count(), 1, "{head}");
    assert_eq!(
        header(head, "content-length"),
        Some(body.len().to_string().as_str())
    );
    assert!(contains(body, b"requesting-user-name\x00\x05adeel"));
    assert!(!contains(body, b"mallory"));
    assert!(body.ends_with(b"\x03RaS2-document"));

    let log = std::fs::read_to_string(&receipts).unwrap();
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["schema_version"], "anytopdf.print-receipt/1");
    assert_eq!(lines[0]["peer"], "127.0.0.1");
    assert_eq!(lines[0]["user"], "adeel");
    assert_eq!(lines[0]["operation"], "Print-Job");
    assert_eq!(lines[0]["job_name"], "Receipt");
}

#[test]
fn chunked_job_with_expect_continue_is_restamped_and_rechunked() {
    let (helper, seen) = fake_helper();
    let front = start_front(helper, &[], None);
    let body = ipp(2, "mallory", b"chunked-document-data");
    let mut request = format!(
        "POST /ipp/print/anytopdf HTTP/1.1\r\nHost: printer.home.example:8631\r\nContent-Type: application/ipp\r\nTransfer-Encoding: chunked\r\nExpect: 100-continue\r\n{}\r\n",
        auth("adeel", "correct horse")
    )
    .into_bytes();
    for piece in body.chunks(7) {
        request.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
        request.extend_from_slice(piece);
        request.extend_from_slice(b"\r\n");
    }
    request.extend_from_slice(b"0\r\n\r\n");
    let response = exchange(front, &request).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    let seen = seen.lock().unwrap();
    assert!(header(&seen[0].head, "expect").is_none());
    assert!(contains(
        &seen[0].body,
        b"requesting-user-name\x00\x05adeel"
    ));
    assert!(seen[0].body.ends_with(b"chunked-document-data"));
}

#[test]
fn signed_in_connection_keeps_its_user_for_later_requests() {
    let (helper, seen) = fake_helper();
    let dir = tempfile::tempdir().unwrap();
    let receipts = dir.path().join("receipts.jsonl");
    let front = start_front(helper, &[], Some(&receipts));
    let mut tls = connect(front).unwrap();
    let mut pending = Vec::new();
    tls.write_all(&post(&ipp(0x0B, "x", b""), &auth("adeel", "correct horse")))
        .unwrap();
    assert!(
        response(&mut tls, &mut pending)
            .unwrap()
            .0
            .starts_with("HTTP/1.1 200")
    );
    tls.write_all(&post(&ipp(2, "mallory", b"DOC"), ""))
        .unwrap();
    assert!(
        response(&mut tls, &mut pending)
            .unwrap()
            .0
            .starts_with("HTTP/1.1 200")
    );
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(
        seen.iter()
            .all(|s| contains(&s.body, b"requesting-user-name\x00\x05adeel"))
    );
    let log = std::fs::read_to_string(&receipts).unwrap();
    assert_eq!(
        log.lines().count(),
        1,
        "only the Print-Job is a receipt: {log}"
    );
}

#[test]
fn malformed_ipp_is_a_bad_request() {
    let (helper, _) = fake_helper();
    let front = start_front(helper, &[], None);
    let request = post(
        &[0, 0, 0, 2, 0, 0, 0, 1, 3],
        &auth("adeel", "correct horse"),
    );
    let response = exchange(front, &request).unwrap();
    assert!(response.starts_with("HTTP/1.1 400"), "{response}");
}

#[test]
fn peer_outside_the_allowlist_is_dropped_before_tls() {
    let (helper, seen) = fake_helper();
    let front = start_front(helper, &["100.64.0.0/10"], None);
    let request = post(&ipp(2, "x", b"DOC"), &auth("adeel", "correct horse"));
    assert!(exchange(front, &request).is_err());
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn unreachable_helper_answers_bad_gateway() {
    let unused = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let front = start_front(unused, &[], None);
    let request = post(&ipp(2, "x", b"DOC"), &auth("adeel", "correct horse"));
    let response = exchange(front, &request).unwrap();
    assert!(response.starts_with("HTTP/1.1 502"), "{response}");
}
