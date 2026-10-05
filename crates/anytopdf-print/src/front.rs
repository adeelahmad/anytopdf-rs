//! The remote front: accepts TLS connections from allowed peers, checks HTTP
//! Basic credentials on the first request, then passes the connection through
//! to the print helper on localhost. Threads and blocking sockets keep it free
//! of an async runtime; printing traffic is a handful of connections.

use crate::allow::Allowlist;
use crate::auth::Users;
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

const UNAUTHORIZED: &[u8] = b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"anytopdf\", charset=\"UTF-8\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const HEAD_TOO_LARGE: &[u8] = b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
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
    let mut tls = TlsPump { conn, tcp };

    let mut head = Vec::new();
    let deadline = Instant::now() + HEAD_TIMEOUT;
    let head_end = loop {
        if let Some(pos) = find(&head, b"\r\n\r\n") {
            break pos + 4;
        }
        if head.len() > MAX_HEAD {
            tls.send(HEAD_TOO_LARGE)?;
            tls.close();
            return Ok(());
        }
        if Instant::now() > deadline || tls.poll(&mut head)? {
            return Ok(());
        }
    };

    if !config.users.is_empty() {
        let header = header_value(&head[..head_end], "authorization");
        match config.users.authorize(header.as_deref()) {
            Some(user) => eprintln!("print remote: {peer}: signed in as {user}"),
            None => {
                if header.is_some() {
                    eprintln!("print remote: {peer}: wrong user name or password");
                    std::thread::sleep(FAILED_AUTH_DELAY);
                }
                tls.send(UNAUTHORIZED)?;
                tls.close();
                return Ok(());
            }
        }
    }

    let mut upstream = match TcpStream::connect_timeout(&config.upstream, CONNECT_TIMEOUT) {
        Ok(stream) => stream,
        Err(e) => {
            tls.send(BAD_GATEWAY)?;
            tls.close();
            return Err(io::Error::new(
                e.kind(),
                format!("print helper at {} is unreachable: {e}", config.upstream),
            ));
        }
    };
    upstream.set_nodelay(true)?;
    upstream.set_read_timeout(Some(POLL))?;
    upstream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    upstream.write_all(&head)?;

    let mut buf = vec![0u8; 16 * 1024];
    let mut client_open = true;
    let mut last = Instant::now();
    loop {
        if client_open {
            let mut data = Vec::new();
            let eof = tls.poll(&mut data)?;
            if !data.is_empty() {
                upstream.write_all(&data)?;
                last = Instant::now();
            }
            if eof {
                client_open = false;
                let _ = upstream.shutdown(Shutdown::Write);
            }
        }
        match upstream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                tls.send(&buf[..n])?;
                last = Instant::now();
            }
            Err(e) if is_timeout(&e) => {}
            Err(e) => return Err(e),
        }
        if last.elapsed() > config.idle_timeout {
            break;
        }
    }
    tls.close();
    Ok(())
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

/// First value of a header in an HTTP request head, matched case-insensitively.
fn header_value(head: &[u8], name: &str) -> Option<String> {
    let text = std::str::from_utf8(head).ok()?;
    text.split("\r\n").skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_lookup_ignores_case_and_request_line() {
        let head = b"POST /ipp/print HTTP/1.1\r\nHost: x\r\nAUTHORIZATION:  Basic abc \r\n\r\n";
        assert_eq!(
            header_value(head, "authorization").as_deref(),
            Some("Basic abc")
        );
        assert_eq!(header_value(head, "expect"), None);
    }
}
