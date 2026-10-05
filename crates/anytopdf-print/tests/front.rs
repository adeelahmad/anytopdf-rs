use anytopdf_print::{Allowlist, Front, FrontConfig, Users, load_tls};
use base64ct::{Base64, Encoding};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A stand-in print helper: records each request and answers `hello`.
fn fake_helper() -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            while !String::from_utf8_lossy(&data).ends_with("IPP!") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => data.extend_from_slice(&buf[..n]),
                }
            }
            log.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&data).into_owned());
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            );
        }
    });
    (addr, seen)
}

fn start_front(upstream: SocketAddr, allow: &[&str]) -> SocketAddr {
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
    })
    .unwrap();
    let addr = front.local_addr().unwrap();
    std::thread::spawn(move || front.serve());
    addr
}

/// Sends one IPP-shaped POST over TLS and returns the response, or the error.
fn request(addr: SocketAddr, credentials: Option<(&str, &str)>) -> Result<String, std::io::Error> {
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
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    let auth = credentials
        .map(|(u, p)| {
            format!(
                "Authorization: Basic {}\r\n",
                Base64::encode_string(format!("{u}:{p}").as_bytes())
            )
        })
        .unwrap_or_default();
    write!(
        tls,
        "POST /ipp/print HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/ipp\r\nContent-Length: 4\r\n{auth}\r\nIPP!"
    )?;
    let mut response = Vec::new();
    match tls.read_to_end(&mut response) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && !response.is_empty() => {}
        Err(e) => return Err(e),
    }
    Ok(String::from_utf8_lossy(&response).into_owned())
}

#[test]
fn request_without_credentials_gets_a_basic_challenge_and_never_reaches_the_helper() {
    let (helper, seen) = fake_helper();
    let front = start_front(helper, &[]);
    let response = request(front, None).unwrap();
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    assert!(response.contains("WWW-Authenticate: Basic realm=\"anytopdf\""));
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn wrong_password_is_refused() {
    let (helper, seen) = fake_helper();
    let front = start_front(helper, &[]);
    let response = request(front, Some(("adeel", "wrong"))).unwrap();
    assert!(response.starts_with("HTTP/1.1 401"), "{response}");
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn signed_in_request_is_passed_through_to_the_helper_and_back() {
    let (helper, seen) = fake_helper();
    let front = start_front(helper, &[]);
    let response = request(front, Some(("adeel", "correct horse"))).unwrap();
    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.ends_with("hello"), "{response}");
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].starts_with("POST /ipp/print HTTP/1.1\r\n"));
    assert!(seen[0].ends_with("\r\n\r\nIPP!"));
}

#[test]
fn peer_outside_the_allowlist_is_dropped_before_tls() {
    let (helper, seen) = fake_helper();
    let front = start_front(helper, &["100.64.0.0/10"]);
    assert!(request(front, Some(("adeel", "correct horse"))).is_err());
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn unreachable_helper_answers_bad_gateway() {
    let unused = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let front = start_front(unused, &[]);
    let response = request(front, Some(("adeel", "correct horse"))).unwrap();
    assert!(response.starts_with("HTTP/1.1 502"), "{response}");
}
