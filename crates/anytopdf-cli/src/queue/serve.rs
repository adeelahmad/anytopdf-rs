//! Opt-in HTTP upload intake: authenticated uploads become queued jobs; job
//! status and finished PDFs can be fetched back. One request per connection,
//! blocking sockets and a thread per connection, like the remote print front.

use super::webhook::job_data;
use super::{Job, JobState, Origin, Queue};
use anyhow::{Context, Result};
use rustls::{ServerConnection, StreamOwned};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const MAX_HEAD: usize = 16 * 1024;
const LINGER_TIMEOUT: Duration = Duration::from_secs(2);
const LINGER_BYTES: usize = 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
const FAILED_AUTH_DELAY: Duration = Duration::from_secs(1);
/// Uploaded names are cut to this many bytes.
const MAX_NAME: usize = 200;
pub(crate) const MIN_TOKEN_LEN: usize = 16;

pub(crate) struct ServeConfig {
    pub(crate) queue: Queue,
    pub(crate) token: Vec<u8>,
    pub(crate) max_upload: u64,
    pub(crate) convert_args: Vec<String>,
    pub(crate) tls: Option<Arc<rustls::ServerConfig>>,
    pub(crate) max_connections: usize,
    pub(crate) quiet: bool,
    /// Index database answering GET /v1/search; `None` leaves search off.
    pub(crate) search_index: Option<std::path::PathBuf>,
}

/// Refuse listeners that would expose the queue without TLS, or on every interface
/// without an explicit opt-in, before any socket is opened.
pub(crate) fn check_exposure(
    listen: SocketAddr,
    tls: bool,
    allow_public_bind: bool,
) -> Result<(), String> {
    if listen.ip().is_loopback() {
        return Ok(());
    }
    if !tls {
        return Err(format!(
            "{listen} is not a loopback address; a non-loopback --listen needs --tls-cert and --tls-key"
        ));
    }
    if listen.ip().is_unspecified() && !allow_public_bind {
        return Err(format!(
            "{listen} listens on every interface; bind one address, or pass --allow-public-bind"
        ));
    }
    Ok(())
}

pub(crate) fn serve(listener: TcpListener, cfg: ServeConfig) -> Result<()> {
    let cfg = Arc::new(cfg);
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let peer = stream
            .peer_addr()
            .map_or_else(|_| "?".into(), |p| p.to_string());
        if active.fetch_add(1, Ordering::SeqCst) >= cfg.max_connections {
            active.fetch_sub(1, Ordering::SeqCst);
            log(&cfg, &format!("refused {peer}: connection limit reached"));
            continue;
        }
        let cfg = Arc::clone(&cfg);
        let active = Arc::clone(&active);
        std::thread::spawn(move || {
            if let Err(e) = connection(stream, &cfg) {
                log(&cfg, &format!("{peer}: {e}"));
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

fn log(cfg: &ServeConfig, line: &str) {
    if !cfg.quiet {
        // A closed stderr must not take a connection thread down.
        let _ = writeln!(io::stderr(), "queue serve: {line}");
    }
}

fn connection(tcp: TcpStream, cfg: &ServeConfig) -> io::Result<()> {
    tcp.set_read_timeout(Some(READ_TIMEOUT))?;
    tcp.set_write_timeout(Some(WRITE_TIMEOUT))?;
    match &cfg.tls {
        Some(tls) => {
            let conn = ServerConnection::new(Arc::clone(tls)).map_err(io::Error::other)?;
            let mut stream = StreamOwned::new(conn, tcp);
            let result = handle(&mut stream, cfg);
            stream.conn.send_close_notify();
            while stream.conn.wants_write() {
                stream.conn.write_tls(&mut stream.sock)?;
            }
            linger(&stream.sock);
            result
        }
        None => {
            let mut tcp = tcp;
            let result = handle(&mut tcp, cfg);
            linger(&tcp);
            result
        }
    }
}

/// Close gracefully: stop writing, then discard what the client is still sending
/// (an unread body after an early answer), so the close is not a reset that
/// destroys the response before the client reads it.
fn linger(tcp: &TcpStream) {
    let _ = tcp.shutdown(std::net::Shutdown::Write);
    let _ = tcp.set_read_timeout(Some(LINGER_TIMEOUT));
    let mut sink = [0u8; 8192];
    let mut drained = 0;
    let mut reader = tcp;
    while drained < LINGER_BYTES {
        match reader.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(n) => drained += n,
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    /// Body bytes that arrived with the head.
    early: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

fn read_head(s: &mut impl Read) -> io::Result<Option<Request>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if buf.len() > MAX_HEAD {
            return Ok(None);
        }
        let n = s.read(&mut chunk)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let Ok(head) = std::str::from_utf8(&buf[..end]) else {
        return Ok(None);
    };
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or_default().split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Ok(None);
    };
    if !version.starts_with("HTTP/1.") {
        return Ok(None);
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut headers = Vec::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Ok(None);
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    Ok(Some(Request {
        method: method.to_string(),
        path: path.to_string(),
        query: query.to_string(),
        headers,
        early: buf[end + 4..].to_vec(),
    }))
}

fn respond(
    s: &mut impl Write,
    status: &str,
    kind: &str,
    body: &[u8],
    extra: &str,
) -> io::Result<()> {
    write!(
        s,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n",
        body.len()
    )?;
    s.write_all(body)?;
    s.flush()
}

fn json(
    s: &mut impl Write,
    status: &str,
    value: &serde_json::Value,
    extra: &str,
) -> io::Result<()> {
    let mut body = serde_json::to_vec(value).map_err(io::Error::other)?;
    body.push(b'\n');
    respond(s, status, "application/json", &body, extra)
}

fn error(s: &mut impl Write, status: &str, message: &str) -> io::Result<()> {
    json(s, status, &serde_json::json!({ "error": message }), "")
}

/// Compare without an early exit, so timing does not reveal a matching prefix.
fn token_matches(given: &[u8], expected: &[u8]) -> bool {
    let mut diff = given.len() ^ expected.len();
    for (i, byte) in expected.iter().enumerate() {
        diff |= usize::from(byte ^ given.get(i).copied().unwrap_or(!byte));
    }
    diff == 0
}

fn handle<S: Read + Write>(s: &mut S, cfg: &ServeConfig) -> io::Result<()> {
    let Some(request) = read_head(s)? else {
        return error(s, "400 Bad Request", "malformed request head");
    };
    let given = request
        .header("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !token_matches(given.trim().as_bytes(), &cfg.token) {
        std::thread::sleep(FAILED_AUTH_DELAY);
        return json(
            s,
            "401 Unauthorized",
            &serde_json::json!({ "error": "missing or wrong bearer token" }),
            "WWW-Authenticate: Bearer realm=\"anytopdf\"\r\n",
        );
    }
    let segments: Vec<&str> = request.path.trim_matches('/').split('/').collect();
    match (request.method.as_str(), segments.as_slice()) {
        ("POST", ["v1", "jobs"]) => upload(s, cfg, &request),
        ("GET", ["v1", "jobs", id]) => status(s, cfg, id),
        ("GET", ["v1", "jobs", id, "output"]) => output(s, cfg, id),
        ("GET", ["v1", "search"]) => search(s, cfg, &request.query),
        (
            _,
            ["v1", "jobs"] | ["v1", "jobs", _] | ["v1", "jobs", _, "output"] | ["v1", "search"],
        ) => error(s, "405 Method Not Allowed", "method not allowed"),
        _ => error(s, "404 Not Found", "no such endpoint"),
    }
}

fn upload<S: Read + Write>(s: &mut S, cfg: &ServeConfig, request: &Request) -> io::Result<()> {
    if request.header("transfer-encoding").is_some() {
        return error(s, "411 Length Required", "send Content-Length, not chunked");
    }
    let Some(length) = request
        .header("content-length")
        .and_then(|v| v.parse::<u64>().ok())
    else {
        return error(s, "411 Length Required", "Content-Length is required");
    };
    if length > cfg.max_upload {
        return error(
            s,
            "413 Content Too Large",
            &format!("uploads are limited to {} bytes", cfg.max_upload),
        );
    }
    if length == 0 {
        return error(s, "400 Bad Request", "the upload is empty");
    }
    let name = upload_name(&query_param(&request.query, "filename").unwrap_or_default());
    if request
        .header("expect")
        .is_some_and(|v| v.eq_ignore_ascii_case("100-continue"))
    {
        s.write_all(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        s.flush()?;
    }

    let mut job = Job::new(
        Origin::Http,
        Vec::new(),
        cfg.convert_args.clone(),
        cfg.queue.root().to_path_buf(),
    );
    let work = cfg.queue.work_dir(&job.id);
    let path = work.join("input").join(&name);
    if let Err(e) = receive(s, request, length, &path) {
        let _ = std::fs::remove_dir_all(&work);
        return match e.kind() {
            io::ErrorKind::UnexpectedEof | io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => {
                error(
                    s,
                    "400 Bad Request",
                    "the body is shorter than Content-Length",
                )
            }
            _ => Err(e),
        };
    }
    job.inputs = vec![path];
    if let Err(e) = cfg.queue.enqueue(&job) {
        let _ = std::fs::remove_dir_all(&work);
        log(cfg, &format!("could not queue upload: {e:#}"));
        return error(s, "500 Internal Server Error", "could not queue the upload");
    }
    log(cfg, &format!("{} queued from upload: {name}", job.id));
    json(
        s,
        "202 Accepted",
        &job_data(&cfg.queue, &job),
        &format!("Location: /v1/jobs/{}\r\n", job.id),
    )
}

/// Write exactly `length` body bytes to `path`.
fn receive(s: &mut impl Read, request: &Request, length: u64, path: &Path) -> io::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")))?;
    let mut file = std::fs::File::create_new(path)?;
    let early = &request.early[..request.early.len().min(length as usize)];
    file.write_all(early)?;
    let rest = length - early.len() as u64;
    let copied = io::copy(&mut s.take(rest), &mut file)?;
    if copied < rest {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    file.sync_all()
}

fn status<S: Write>(s: &mut S, cfg: &ServeConfig, id: &str) -> io::Result<()> {
    match find(cfg, id) {
        Some(job) => json(s, "200 OK", &job_data(&cfg.queue, &job), ""),
        None => error(s, "404 Not Found", "no such job"),
    }
}

fn output<S: Write>(s: &mut S, cfg: &ServeConfig, id: &str) -> io::Result<()> {
    let Some(job) = find(cfg, id) else {
        return error(s, "404 Not Found", "no such job");
    };
    let file = job
        .output
        .as_deref()
        .filter(|_| job.state == JobState::Succeeded)
        .and_then(|p| std::fs::File::open(p).ok());
    let Some(mut file) = file else {
        return error(
            s,
            "409 Conflict",
            &format!("job is {}; no output to download", job.state.as_str()),
        );
    };
    let length = file.metadata()?.len();
    write!(
        s,
        "HTTP/1.1 200 OK\r\nContent-Type: application/pdf\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
    )?;
    io::copy(&mut file, s)?;
    s.flush()
}

/// `GET /v1/search?q=…&kind=…&person=…&collection=…&limit=…`: the same
/// `anytopdf.search/1` document as `anytopdf search --json`, without the index path.
fn search<S: Write>(s: &mut S, cfg: &ServeConfig, query: &str) -> io::Result<()> {
    let Some(path) = &cfg.search_index else {
        return error(
            s,
            "404 Not Found",
            "search is not enabled; start queue serve with --search",
        );
    };
    let kinds: Vec<String> = query_params(query, "kind")
        .iter()
        .flat_map(|k| k.split(','))
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect();
    if let Some(bad) = kinds
        .iter()
        .find(|k| !crate::cli::ENTRY_KINDS.contains(&k.as_str()))
    {
        return error(s, "400 Bad Request", &format!("unknown kind {bad:?}"));
    }
    let limit = match query_param(query, "limit").map(|l| l.parse::<usize>()) {
        None => 20,
        Some(Ok(n)) if (1..=1000).contains(&n) => n,
        Some(_) => return error(s, "400 Bad Request", "limit must be between 1 and 1000"),
    };
    let text = query_param(query, "q").filter(|q| !q.trim().is_empty());
    let request = anytopdf_index::SearchQuery {
        text: text.clone(),
        kinds,
        person: query_param(query, "person").filter(|p| !p.is_empty()),
        collection: query_param(query, "collection").filter(|c| !c.is_empty()),
        limit,
    };
    if request.text.is_none()
        && request.kinds.is_empty()
        && request.person.is_none()
        && request.collection.is_none()
    {
        return error(
            s,
            "400 Bad Request",
            "give q, or filter with kind, person or collection",
        );
    }
    let hits = anytopdf_index::Index::open_existing(path).and_then(|index| index.search(&request));
    match hits {
        Ok(hits) => json(
            s,
            "200 OK",
            &serde_json::json!({
                "schema_version": crate::search::SEARCH_SCHEMA,
                "query": text,
                "hits": hits,
            }),
            "",
        ),
        Err(e) => {
            log(cfg, &format!("search failed: {e:#}"));
            error(
                s,
                "503 Service Unavailable",
                "the search index is not available",
            )
        }
    }
}

fn find(cfg: &ServeConfig, id: &str) -> Option<Job> {
    let valid = id.strip_prefix("job_").is_some_and(|hex| {
        hex.len() == 32
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    if !valid {
        return None;
    }
    cfg.queue.find(id).ok().flatten()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query_params(query, key).into_iter().next()
}

fn query_params(query: &str, key: &str) -> Vec<String> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(k, _)| *k == key)
        .map(|(_, v)| percent_decode(v))
        .collect()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16);
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push((h * 16 + l) as u8);
                    i += 3;
                    continue;
                }
                _ => out.push(b'%'),
            },
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A safe file name for an upload: the last path component, without control
/// characters or a leading dot, at most `MAX_NAME` bytes.
pub(crate) fn upload_name(requested: &str) -> String {
    let base = requested.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    let mut name = String::new();
    for c in cleaned.chars() {
        if name.len() + c.len_utf8() > MAX_NAME {
            break;
        }
        name.push(c);
    }
    if name.is_empty() {
        "upload".into()
    } else {
        name
    }
}

pub(crate) fn bind(listen: SocketAddr) -> Result<TcpListener> {
    TcpListener::bind(listen).with_context(|| format!("cannot listen on {listen}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_names_cannot_escape_the_job_directory() {
        assert_eq!(upload_name("../../etc/passwd"), "passwd");
        assert_eq!(upload_name(r"..\..\boot.ini"), "boot.ini");
        assert_eq!(upload_name(".hidden.txt"), "hidden.txt");
        assert_eq!(upload_name(".."), "upload");
        assert_eq!(upload_name(""), "upload");
        assert_eq!(upload_name("a\u{0}b\nc:d.txt"), "abcd.txt");
        assert_eq!(upload_name(&"é".repeat(300)).len(), MAX_NAME);
        assert_eq!(upload_name("scan 1.jpg"), "scan 1.jpg");
    }

    #[test]
    fn query_values_are_percent_decoded() {
        assert_eq!(
            query_param("x=1&filename=my%20scan%2B1.jpg", "filename").as_deref(),
            Some("my scan+1.jpg")
        );
        assert_eq!(
            query_param("filename=a+b%2", "filename").as_deref(),
            Some("a b%2")
        );
        assert_eq!(query_param("other=1", "filename"), None);
    }

    #[test]
    fn tokens_compare_whole_values() {
        assert!(token_matches(b"0123456789abcdef", b"0123456789abcdef"));
        assert!(!token_matches(b"0123456789abcde", b"0123456789abcdef"));
        assert!(!token_matches(b"0123456789abcdefX", b"0123456789abcdef"));
        assert!(!token_matches(b"", b"0123456789abcdef"));
        assert!(!token_matches(b"1123456789abcdef", b"0123456789abcdef"));
    }

    #[test]
    fn listeners_off_loopback_need_tls_and_an_opt_in_for_every_interface() {
        let addr = |s: &str| s.parse::<SocketAddr>().unwrap();
        assert!(check_exposure(addr("127.0.0.1:8640"), false, false).is_ok());
        assert!(check_exposure(addr("[::1]:8640"), false, false).is_ok());
        assert!(check_exposure(addr("100.64.1.2:8640"), false, false).is_err());
        assert!(check_exposure(addr("100.64.1.2:8640"), true, false).is_ok());
        assert!(check_exposure(addr("0.0.0.0:8640"), true, false).is_err());
        assert!(check_exposure(addr("[::]:8640"), true, true).is_ok());
    }
}
