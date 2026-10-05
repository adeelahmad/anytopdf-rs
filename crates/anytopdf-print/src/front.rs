//! The remote front: accepts TLS connections from allowed peers, checks HTTP
//! Basic credentials, stamps the signed-in user on each IPP request and
//! forwards requests to the print helper on localhost. Threads and blocking sockets keep it free
//! of an async runtime; printing traffic is a handful of connections.

use crate::allow::Allowlist;
use crate::auth::Users;
use crate::ipp;
use crate::receipts::Receipts;
use anyhow::{Context, Result, bail};
use rustls::ServerConnection;
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer};
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const POLL: Duration = Duration::from_millis(20);
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_TIMEOUT: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const FAILED_AUTH_DELAY: Duration = Duration::from_secs(1);
const MAX_HEAD: usize = 16 * 1024;
const MAX_ATTRIBUTES: usize = 64 * 1024;

const UNAUTHORIZED: &[u8] = b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"anytopdf\", charset=\"UTF-8\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const HEAD_TOO_LARGE: &[u8] = b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_REQUEST: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_GATEWAY: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

pub struct FrontConfig {
    pub listen: SocketAddr,
    /// The print helper's loopback address.
    pub upstream: SocketAddr,
    pub tls: Arc<rustls::ServerConfig>,
    /// With no users, requests are passed through unauthenticated; the guard
    /// only allows that on a loopback listener.
    pub users: Arc<Users>,
    /// With an empty allowlist only loopback peers are admitted.
    pub allow: Allowlist,
    pub max_connections: usize,
    pub idle_timeout: Duration,
    /// Appends one line per submitted job when set.
    pub receipts: Option<Receipts>,
}

/// Loads a PEM certificate chain and private key (for example the files
/// `tailscale cert` writes) into a TLS 1.2/1.3 server configuration.
pub fn load_tls(cert: &Path, key: &Path) -> Result<Arc<rustls::ServerConfig>> {
    let certs = CertificateDer::pem_file_iter(cert)
        .with_context(|| format!("cannot read certificate {}", cert.display()))?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| format!("cannot parse certificate {}", cert.display()))?;
    if certs.is_empty() {
        bail!("no certificate found in {}", cert.display());
    }
    let key = PrivateKeyDer::from_pem_file(key)
        .with_context(|| format!("cannot read private key {}", key.display()))?;
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .context("certificate and private key do not form a valid TLS identity")?;
    Ok(Arc::new(config))
}

pub struct Front {
    listener: TcpListener,
    config: Arc<FrontConfig>,
    active: Arc<AtomicUsize>,
}

impl Front {
    pub fn bind(config: FrontConfig) -> Result<Front> {
        let listener = TcpListener::bind(config.listen)
            .with_context(|| format!("cannot listen on {}", config.listen))?;
        Ok(Front {
            listener,
            config: Arc::new(config),
            active: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// Accepts connections until the listener fails; each runs on its own thread.
    pub fn serve(self) -> Result<()> {
        for stream in self.listener.incoming() {
            let Ok(stream) = stream else { continue };
            let Ok(peer) = stream.peer_addr() else {
                continue;
            };
            if !self.admits(peer) {
                eprintln!("print remote: refused {peer}: not in the allowlist");
                continue;
            }
            if self.active.fetch_add(1, Ordering::SeqCst) >= self.config.max_connections {
                self.active.fetch_sub(1, Ordering::SeqCst);
                eprintln!("print remote: refused {peer}: connection limit reached");
                continue;
            }
            let config = Arc::clone(&self.config);
            let active = Arc::clone(&self.active);
            std::thread::spawn(move || {
                if let Err(e) = handle(stream, peer, &config) {
                    eprintln!("print remote: {peer}: {e}");
                }
                active.fetch_sub(1, Ordering::SeqCst);
            });
        }
        Ok(())
    }

    fn admits(&self, peer: SocketAddr) -> bool {
        let allow = &self.config.allow;
        if allow.is_empty() {
            peer.ip().is_loopback()
        } else {
            allow.permits(peer.ip())
        }
    }
}

fn handle(tcp: TcpStream, peer: SocketAddr, config: &FrontConfig) -> io::Result<()> {
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(POLL))?;
    tcp.set_write_timeout(Some(WRITE_TIMEOUT))?;
    let conn = ServerConnection::new(Arc::clone(&config.tls)).map_err(io::Error::other)?;
    let mut session = Session {
        tls: TlsPump { conn, tcp },
        peer,
        config,
        upstream: None,
        stage: Stage::Head,
        user: None,
        verified_header: None,
    };
    let result = session.run();
    session.tls.close();
    result
}

/// Where the client-to-helper stream is: reading a request head, collecting
/// IPP attributes to stamp, or forwarding the rest of a body.
enum Stage {
    Head,
    Attributes(Pending),
    Body { framing: Framing, chunked: bool },
}

struct Pending {
    head: Vec<String>,
    framing: Framing,
    declared_len: Option<u64>,
    decoded: Vec<u8>,
}

enum Step {
    Progress,
    Wait,
    /// Send this response to the client and close.
    Reject(&'static [u8]),
}

struct Session<'a> {
    tls: TlsPump,
    peer: SocketAddr,
    config: &'a FrontConfig,
    upstream: Option<TcpStream>,
    stage: Stage,
    user: Option<String>,
    verified_header: Option<String>,
}

impl Session<'_> {
    fn run(&mut self) -> io::Result<()> {
        let mut inbuf = Vec::new();
        let mut buf = vec![0u8; 16 * 1024];
        let mut client_open = true;
        let mut last = Instant::now();
        let started = Instant::now();
        loop {
            if client_open {
                let before = inbuf.len();
                client_open = !self.tls.poll(&mut inbuf)?;
                if inbuf.len() != before {
                    last = Instant::now();
                }
            }
            loop {
                match self.step(&mut inbuf)? {
                    Step::Progress => {}
                    Step::Wait => break,
                    Step::Reject(response) => {
                        self.tls.send(response)?;
                        return Ok(());
                    }
                }
            }
            if !client_open {
                if !matches!(self.stage, Stage::Head) || !inbuf.is_empty() {
                    return Ok(());
                }
                match &self.upstream {
                    Some(up) => {
                        let _ = up.shutdown(Shutdown::Write);
                    }
                    None => return Ok(()),
                }
            }
            if self.upstream.is_none() && self.user.is_none() && !self.config.users.is_empty() {
                if started.elapsed() > HEAD_TIMEOUT {
                    return Ok(());
                }
                continue;
            }
            if let Some(up) = self.upstream.as_mut() {
                match up.read(&mut buf) {
                    Ok(0) => return Ok(()),
                    Ok(n) => {
                        self.tls.send(&buf[..n])?;
                        last = Instant::now();
                    }
                    Err(e) if is_timeout(&e) => {}
                    Err(e) => return Err(e),
                }
            }
            if last.elapsed() > self.config.idle_timeout {
                return Ok(());
            }
        }
    }

    fn step(&mut self, inbuf: &mut Vec<u8>) -> io::Result<Step> {
        match std::mem::replace(&mut self.stage, Stage::Head) {
            Stage::Head => self.head(inbuf),
            Stage::Attributes(mut pending) => {
                let done = pending.framing.decode(inbuf, &mut pending.decoded)?;
                let end = match ipp::attributes_len(&pending.decoded) {
                    Ok(Some(end)) => end,
                    Ok(None) if done || pending.decoded.len() > MAX_ATTRIBUTES => {
                        return Ok(Step::Reject(BAD_REQUEST));
                    }
                    Ok(None) => {
                        self.stage = Stage::Attributes(pending);
                        return Ok(Step::Wait);
                    }
                    Err(_) => return Ok(Step::Reject(BAD_REQUEST)),
                };
                let Ok((attributes, summary)) =
                    ipp::stamp_user(&pending.decoded[..end], self.user.as_deref())
                else {
                    return Ok(Step::Reject(BAD_REQUEST));
                };
                if summary.submits_job() {
                    eprintln!(
                        "print remote: {}: {} from {}",
                        self.peer.ip(),
                        summary.operation_name(),
                        self.user.as_deref().unwrap_or("(no user)")
                    );
                    if let Some(receipts) = &self.config.receipts {
                        receipts.record(self.peer, self.user.as_deref(), &summary);
                    }
                }
                let chunked = matches!(pending.framing, Framing::Chunked(_));
                let mut head = pending.head;
                match pending.declared_len {
                    Some(len) => head.push(format!(
                        "Content-Length: {}",
                        len - end as u64 + attributes.len() as u64
                    )),
                    None => head.push("Transfer-Encoding: chunked".into()),
                }
                let mut out = head_bytes(&head);
                let mut body = attributes;
                body.extend_from_slice(&pending.decoded[end..]);
                encode(&mut out, &body, chunked, done);
                self.upstream()?.write_all(&out)?;
                if !done {
                    self.stage = Stage::Body {
                        framing: pending.framing,
                        chunked,
                    };
                }
                Ok(Step::Progress)
            }
            Stage::Body {
                mut framing,
                chunked,
            } => {
                let mut data = Vec::new();
                let done = framing.decode(inbuf, &mut data)?;
                if data.is_empty() && !done {
                    self.stage = Stage::Body { framing, chunked };
                    return Ok(Step::Wait);
                }
                let mut out = Vec::with_capacity(data.len() + 16);
                encode(&mut out, &data, chunked, done);
                self.upstream()?.write_all(&out)?;
                if !done {
                    self.stage = Stage::Body { framing, chunked };
                }
                Ok(Step::Progress)
            }
        }
    }

    fn head(&mut self, inbuf: &mut Vec<u8>) -> io::Result<Step> {
        let Some(pos) = find(inbuf, b"\r\n\r\n") else {
            if inbuf.len() > MAX_HEAD {
                return Ok(Step::Reject(HEAD_TOO_LARGE));
            }
            return Ok(Step::Wait);
        };
        let raw: Vec<u8> = inbuf.drain(..pos + 4).collect();
        let Ok(text) = std::str::from_utf8(&raw[..pos]) else {
            return Ok(Step::Reject(BAD_REQUEST));
        };
        let mut lines = text.split("\r\n");
        let request_line = lines.next().unwrap_or_default().to_string();
        let headers: Vec<(&str, &str)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim(), v.trim()))
            .collect();
        let get = |name: &str| {
            headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| *v)
        };

        if !self.config.users.is_empty() {
            match get("authorization") {
                Some(h) if self.verified_header.as_deref() == Some(h) => {}
                Some(h) => match self.config.users.authorize(Some(h)) {
                    Some(user) => {
                        if self.user.as_deref() != Some(&user) {
                            eprintln!("print remote: {}: signed in as {user}", self.peer.ip());
                        }
                        self.user = Some(user);
                        self.verified_header = Some(h.to_string());
                    }
                    None => {
                        eprintln!(
                            "print remote: {}: wrong user name or password",
                            self.peer.ip()
                        );
                        std::thread::sleep(FAILED_AUTH_DELAY);
                        return Ok(Step::Reject(UNAUTHORIZED));
                    }
                },
                None if self.user.is_some() => {}
                None => return Ok(Step::Reject(UNAUTHORIZED)),
            }
        }

        let chunked =
            get("transfer-encoding").is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
        let declared_len = if chunked {
            None
        } else {
            match get("content-length").map(|v| v.parse::<u64>()) {
                Some(Ok(n)) => Some(n),
                Some(Err(_)) => return Ok(Step::Reject(BAD_REQUEST)),
                None => Some(0),
            }
        };
        let framing = match declared_len {
            Some(n) => Framing::Length(n),
            None => Framing::Chunked(ChunkDecoder::default()),
        };
        if get("expect").is_some_and(|v| v.eq_ignore_ascii_case("100-continue")) {
            self.tls.send(b"HTTP/1.1 100 Continue\r\n\r\n")?;
        }
        if self.upstream.is_none() {
            match TcpStream::connect_timeout(&self.config.upstream, CONNECT_TIMEOUT) {
                Ok(up) => {
                    up.set_nodelay(true)?;
                    up.set_read_timeout(Some(POLL))?;
                    up.set_write_timeout(Some(WRITE_TIMEOUT))?;
                    self.upstream = Some(up);
                }
                Err(e) => {
                    eprintln!(
                        "print remote: print helper at {} is unreachable: {e}",
                        self.config.upstream
                    );
                    return Ok(Step::Reject(BAD_GATEWAY));
                }
            }
        }

        let mut head = vec![request_line.clone()];
        head.extend(
            headers
                .iter()
                .filter(|(k, _)| {
                    ![
                        "authorization",
                        "expect",
                        "content-length",
                        "transfer-encoding",
                    ]
                    .iter()
                    .any(|drop| k.eq_ignore_ascii_case(drop))
                })
                .map(|(k, v)| format!("{k}: {v}")),
        );
        let is_ipp = request_line.starts_with("POST ")
            && get("content-type")
                .is_some_and(|v| v.to_ascii_lowercase().starts_with("application/ipp"));
        if is_ipp && declared_len != Some(0) {
            self.stage = Stage::Attributes(Pending {
                head,
                framing,
                declared_len,
                decoded: Vec::new(),
            });
            return Ok(Step::Progress);
        }
        match declared_len {
            Some(n) => head.push(format!("Content-Length: {n}")),
            None => head.push("Transfer-Encoding: chunked".into()),
        }
        self.upstream()?.write_all(&head_bytes(&head))?;
        if declared_len != Some(0) {
            self.stage = Stage::Body { framing, chunked };
        }
        Ok(Step::Progress)
    }

    fn upstream(&mut self) -> io::Result<&mut TcpStream> {
        self.upstream
            .as_mut()
            .ok_or_else(|| io::Error::other("no upstream connection"))
    }
}

fn head_bytes(lines: &[String]) -> Vec<u8> {
    let mut out = lines.join("\r\n").into_bytes();
    out.extend_from_slice(b"\r\n\r\n");
    out
}

/// Appends body bytes in the outgoing framing; a chunked body ends with the
/// zero-length chunk once `last` is set.
fn encode(out: &mut Vec<u8>, data: &[u8], chunked: bool, last: bool) {
    if !chunked {
        out.extend_from_slice(data);
        return;
    }
    if !data.is_empty() {
        out.extend_from_slice(format!("{:x}\r\n", data.len()).as_bytes());
        out.extend_from_slice(data);
        out.extend_from_slice(b"\r\n");
    }
    if last {
        out.extend_from_slice(b"0\r\n\r\n");
    }
}

enum Framing {
    Length(u64),
    Chunked(ChunkDecoder),
}

impl Framing {
    /// Moves decoded body bytes from the front of `input` into `out`; true once
    /// the body is complete.
    fn decode(&mut self, input: &mut Vec<u8>, out: &mut Vec<u8>) -> io::Result<bool> {
        match self {
            Framing::Length(remaining) => {
                let take = (*remaining).min(input.len() as u64) as usize;
                out.extend(input.drain(..take));
                *remaining -= take as u64;
                Ok(*remaining == 0)
            }
            Framing::Chunked(decoder) => decoder.decode(input, out),
        }
    }
}

#[derive(Default)]
enum ChunkState {
    #[default]
    Size,
    Data(u64),
    DataEnd,
    Trailer,
}

#[derive(Default)]
struct ChunkDecoder {
    state: ChunkState,
}

impl ChunkDecoder {
    fn decode(&mut self, input: &mut Vec<u8>, out: &mut Vec<u8>) -> io::Result<bool> {
        let bad = || io::Error::new(io::ErrorKind::InvalidData, "malformed chunked body");
        loop {
            match self.state {
                ChunkState::Size => {
                    let Some(i) = find(input, b"\r\n") else {
                        return if input.len() > 1024 {
                            Err(bad())
                        } else {
                            Ok(false)
                        };
                    };
                    let line = std::str::from_utf8(&input[..i]).map_err(|_| bad())?;
                    let size = line.split(';').next().unwrap_or_default().trim();
                    let size = u64::from_str_radix(size, 16).map_err(|_| bad())?;
                    input.drain(..i + 2);
                    self.state = if size == 0 {
                        ChunkState::Trailer
                    } else {
                        ChunkState::Data(size)
                    };
                }
                ChunkState::Data(remaining) => {
                    if input.is_empty() {
                        return Ok(false);
                    }
                    let take = remaining.min(input.len() as u64) as usize;
                    out.extend(input.drain(..take));
                    self.state = if take as u64 == remaining {
                        ChunkState::DataEnd
                    } else {
                        ChunkState::Data(remaining - take as u64)
                    };
                }
                ChunkState::DataEnd => {
                    if input.len() < 2 {
                        return Ok(false);
                    }
                    if &input[..2] != b"\r\n" {
                        return Err(bad());
                    }
                    input.drain(..2);
                    self.state = ChunkState::Size;
                }
                ChunkState::Trailer => {
                    let Some(i) = find(input, b"\r\n") else {
                        return if input.len() > MAX_HEAD {
                            Err(bad())
                        } else {
                            Ok(false)
                        };
                    };
                    input.drain(..i + 2);
                    if i == 0 {
                        self.state = ChunkState::Size;
                        return Ok(true);
                    }
                }
            }
        }
    }
}

/// Drives a rustls server connection over a TCP socket with a short read
/// timeout, so one thread can alternate between client and upstream.
struct TlsPump {
    conn: ServerConnection,
    tcp: TcpStream,
}

impl TlsPump {
    fn flush(&mut self) -> io::Result<()> {
        while self.conn.wants_write() {
            self.conn.write_tls(&mut self.tcp)?;
        }
        Ok(())
    }

    /// Appends plaintext that arrives within one poll interval; true at end of stream.
    fn poll(&mut self, out: &mut Vec<u8>) -> io::Result<bool> {
        self.flush()?;
        let mut eof = false;
        match self.conn.read_tls(&mut self.tcp) {
            Ok(0) => eof = true,
            Ok(_) => {}
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e),
        }
        let state = match self.conn.process_new_packets() {
            Ok(state) => state,
            Err(e) => {
                let _ = self.flush();
                return Err(io::Error::new(io::ErrorKind::InvalidData, e));
            }
        };
        let available = state.plaintext_bytes_to_read();
        if available > 0 {
            let start = out.len();
            out.resize(start + available, 0);
            self.conn.reader().read_exact(&mut out[start..])?;
        }
        if state.peer_has_closed() {
            eof = true;
        }
        self.flush()?;
        Ok(eof)
    }

    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        self.conn.writer().write_all(data)?;
        self.flush()
    }

    fn close(&mut self) {
        self.conn.send_close_notify();
        let _ = self.flush();
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_bodies_decode_across_reads_and_skip_trailers() {
        let mut decoder = ChunkDecoder::default();
        let mut out = Vec::new();
        let mut input = b"4;ext=1\r\nWiki\r\n5\r\npe".to_vec();
        assert!(!decoder.decode(&mut input, &mut out).unwrap());
        input.extend_from_slice(b"dia\r\n0\r\nX-Trailer: 1\r\n\r\nNEXT");
        assert!(decoder.decode(&mut input, &mut out).unwrap());
        assert_eq!(out, b"Wikipedia");
        assert_eq!(input, b"NEXT");
    }

    #[test]
    fn malformed_chunk_sizes_are_errors() {
        let mut out = Vec::new();
        assert!(
            ChunkDecoder::default()
                .decode(&mut b"zz\r\n".to_vec(), &mut out)
                .is_err()
        );
        assert!(
            ChunkDecoder::default()
                .decode(&mut b"1\r\nabc\r\n".to_vec(), &mut out)
                .is_err()
        );
    }

    #[test]
    fn chunked_encoding_ends_with_the_zero_chunk() {
        let mut out = Vec::new();
        encode(&mut out, b"abc", true, false);
        encode(&mut out, b"", true, true);
        assert_eq!(out, b"3\r\nabc\r\n0\r\n\r\n");
    }
}
