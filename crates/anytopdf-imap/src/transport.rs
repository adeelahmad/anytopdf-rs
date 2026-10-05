use crate::config::{ImapConfig, TlsMode};
use crate::watcher::{Mailbox, MailboxStatus};
use anyhow::{Context, Result, anyhow, bail};
use imap::extensions::idle::{SetReadTimeout, stop_on_any};
use rustls::pki_types::{CertificateDer, ServerName, pem::PemObject};
use std::{
    io::{Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::Path,
    sync::Arc,
    time::Duration,
};

type TlsStream = rustls::StreamOwned<rustls::ClientConnection, TcpStream>;

/// A plain or TLS socket that never blocks forever: clearing the read timeout (as
/// IDLE does when it finishes) restores the configured I/O timeout instead.
pub struct ImapStream {
    inner: Inner,
    io_timeout: Duration,
}

enum Inner {
    Plain(TcpStream),
    Tls(Box<TlsStream>),
}

impl ImapStream {
    fn tcp(&self) -> &TcpStream {
        match &self.inner {
            Inner::Plain(tcp) => tcp,
            Inner::Tls(tls) => &tls.sock,
        }
    }
}

impl Read for ImapStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match &mut self.inner {
            Inner::Plain(s) => s.read(buf),
            Inner::Tls(s) => s.read(buf),
        }
    }
}

impl Write for ImapStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match &mut self.inner {
            Inner::Plain(s) => s.write(buf),
            Inner::Tls(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match &mut self.inner {
            Inner::Plain(s) => s.flush(),
            Inner::Tls(s) => s.flush(),
        }
    }
}

impl SetReadTimeout for ImapStream {
    fn set_read_timeout(&mut self, timeout: Option<Duration>) -> imap::Result<()> {
        self.tcp()
            .set_read_timeout(Some(timeout.unwrap_or(self.io_timeout)))
            .map_err(imap::Error::Io)
    }
}

/// A logged-in IMAP session watching one mailbox.
pub struct ImapMailbox {
    session: imap::Session<ImapStream>,
    mailbox: String,
    search: Option<String>,
    idle: bool,
    can_move: bool,
    uidplus: bool,
}

/// Connect, secure, log in and probe server capabilities.
pub fn connect(config: &ImapConfig) -> Result<ImapMailbox> {
    config.validate()?;
    let tcp = open_tcp(&config.host, config.port, config.io_timeout)?;
    let tls = || tls_config(config.ca_file.as_deref());
    let stream = |inner| ImapStream {
        inner,
        io_timeout: config.io_timeout,
    };
    let client = match config.tls {
        TlsMode::None => {
            let mut client = imap::Client::new(stream(Inner::Plain(tcp)));
            client.read_greeting().context("read IMAP greeting")?;
            client
        }
        TlsMode::Implicit => {
            let mut client =
                imap::Client::new(stream(Inner::Tls(handshake(tcp, &config.host, tls()?)?)));
            client.read_greeting().context("read IMAP greeting")?;
            client
        }
        TlsMode::StartTls => {
            let tcp = starttls(tcp).with_context(|| format!("STARTTLS with {}", config.host))?;
            // The greeting was consumed before the upgrade; none follows the handshake.
            imap::Client::new(stream(Inner::Tls(handshake(tcp, &config.host, tls()?)?)))
        }
    };
    let mut session = client
        .login(&config.user, &config.password)
        .map_err(|(e, _)| anyhow!(e))
        .with_context(|| format!("IMAP login as {} on {}", config.user, config.host))?;
    let caps = session.capabilities().context("IMAP CAPABILITY")?;
    Ok(ImapMailbox {
        idle: caps.has_str("IDLE"),
        can_move: caps.has_str("MOVE"),
        uidplus: caps.has_str("UIDPLUS"),
        session,
        mailbox: config.mailbox.clone(),
        search: config.search.clone(),
    })
}

fn open_tcp(host: &str, port: u16, timeout: Duration) -> Result<TcpStream> {
    let addrs = (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolve {host}"))?;
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, timeout) {
            Ok(tcp) => {
                tcp.set_read_timeout(Some(timeout))?;
                tcp.set_write_timeout(Some(timeout))?;
                return Ok(tcp);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(match last {
        Some(e) => anyhow!(e).context(format!("connect to {host}:{port}")),
        None => anyhow!("{host} resolved to no addresses"),
    })
}

/// Run the plaintext half of STARTTLS by hand: greeting, `STARTTLS`, tagged `OK`.
/// Reading byte by byte guarantees nothing past the `OK` line is buffered, so the TLS
/// handshake starts on a clean socket.
fn starttls(mut tcp: TcpStream) -> Result<TcpStream> {
    let greeting = read_line(&mut tcp)?;
    if !greeting.starts_with("* OK") {
        bail!("unexpected greeting: {}", greeting.trim_end());
    }
    tcp.write_all(b"s0 STARTTLS\r\n")?;
    loop {
        let line = read_line(&mut tcp)?;
        if line.starts_with("* ") {
            continue;
        }
        if line.starts_with("s0 OK") {
            return Ok(tcp);
        }
        bail!("server refused STARTTLS: {}", line.trim_end());
    }
}

fn read_line(tcp: &mut TcpStream) -> Result<String> {
    const MAX_LINE: usize = 8192;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while !line.ends_with(b"\n") {
        if tcp.read(&mut byte)? == 0 {
            bail!("connection closed");
        }
        line.push(byte[0]);
        if line.len() > MAX_LINE {
            bail!("response line longer than {MAX_LINE} bytes");
        }
    }
    Ok(String::from_utf8_lossy(&line).into_owned())
}

fn tls_config(ca_file: Option<&Path>) -> Result<Arc<rustls::ClientConfig>> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add_parsable_certificates(rustls_native_certs::load_native_certs().certs);
    if let Some(path) = ca_file {
        for cert in CertificateDer::pem_file_iter(path)
            .with_context(|| format!("read --ca-file {}", path.display()))?
        {
            let cert = cert.with_context(|| format!("parse --ca-file {}", path.display()))?;
            roots
                .add(cert)
                .with_context(|| format!("trust --ca-file {}", path.display()))?;
        }
    }
    if roots.is_empty() {
        bail!("no trusted root certificates found on this system; pass --ca-file");
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Complete the handshake up front so certificate problems surface as connection errors.
fn handshake(
    tcp: TcpStream,
    host: &str,
    config: Arc<rustls::ClientConfig>,
) -> Result<Box<TlsStream>> {
    let name = ServerName::try_from(host.to_string())
        .with_context(|| format!("{host} is not a valid TLS server name"))?;
    let conn = rustls::ClientConnection::new(config, name)?;
    let mut tls = Box::new(rustls::StreamOwned::new(conn, tcp));
    while tls.conn.is_handshaking() {
        tls.conn
            .complete_io(&mut tls.sock)
            .with_context(|| format!("TLS handshake with {host}"))?;
    }
    Ok(tls)
}

impl Mailbox for ImapMailbox {
    fn status(&mut self) -> Result<MailboxStatus> {
        let selected = self
            .session
            .select(&self.mailbox)
            .with_context(|| format!("select mailbox {:?}", self.mailbox))?;
        Ok(MailboxStatus {
            uid_validity: selected
                .uid_validity
                .context("server did not report UIDVALIDITY")?,
            uid_next: selected.uid_next,
        })
    }

    fn uids_after(&mut self, last_uid: u32) -> Result<Vec<u32>> {
        let mut query = format!("UID {}:*", last_uid.saturating_add(1));
        if let Some(search) = &self.search {
            query.push(' ');
            query.push_str(search);
        }
        let found = self
            .session
            .uid_search(&query)
            .with_context(|| format!("UID SEARCH {query}"))?;
        // `n:*` always matches the highest UID, even when it is below n.
        let mut uids: Vec<u32> = found.into_iter().filter(|uid| *uid > last_uid).collect();
        uids.sort_unstable();
        Ok(uids)
    }

    fn size(&mut self, uid: u32) -> Result<Option<u64>> {
        let fetches = self
            .session
            .uid_fetch(uid.to_string(), "RFC822.SIZE")
            .with_context(|| format!("fetch size of uid {uid}"))?;
        Ok(fetches
            .iter()
            .find(|f| f.uid == Some(uid))
            .map(|f| f.size.map_or(0, u64::from)))
    }

    fn fetch(&mut self, uid: u32) -> Result<Option<Vec<u8>>> {
        let fetches = self
            .session
            .uid_fetch(uid.to_string(), "BODY.PEEK[]")
            .with_context(|| format!("fetch uid {uid}"))?;
        Ok(fetches
            .iter()
            .find(|f| f.uid == Some(uid))
            .and_then(|f| f.body())
            .map(<[u8]>::to_vec))
    }

    fn mark_seen(&mut self, uid: u32) -> Result<()> {
        self.session
            .uid_store(uid.to_string(), "+FLAGS.SILENT (\\Seen)")
            .with_context(|| format!("mark uid {uid} seen"))?;
        Ok(())
    }

    fn move_to(&mut self, uid: u32, folder: &str) -> Result<()> {
        let set = uid.to_string();
        if self.can_move {
            self.session.uid_mv(&set, folder)?;
            return Ok(());
        }
        self.session.uid_copy(&set, folder)?;
        self.session.uid_store(&set, "+FLAGS.SILENT (\\Deleted)")?;
        // Without UIDPLUS, a plain EXPUNGE could remove other clients' deleted mail,
        // so the original stays flagged \Deleted until the user's client expunges it.
        if self.uidplus {
            self.session.uid_expunge(&set)?;
        }
        Ok(())
    }

    fn wait(&mut self, timeout: Duration) -> Result<()> {
        if self.idle {
            self.session
                .idle()
                .timeout(timeout)
                .keepalive(false)
                .wait_while(stop_on_any)
                .context("IMAP IDLE")?;
        } else {
            std::thread::sleep(timeout);
        }
        Ok(())
    }
}

impl Drop for ImapMailbox {
    fn drop(&mut self) {
        let _ = self.session.logout();
    }
}
