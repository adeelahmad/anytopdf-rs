use anyhow::{Context, Result, bail};
use std::{path::PathBuf, str::FromStr, time::Duration};

/// How the connection is secured.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TlsMode {
    /// TLS from the first byte (IMAPS, usually port 993).
    Implicit,
    /// Plain connection upgraded with `STARTTLS` before login (usually port 143).
    StartTls,
    /// No encryption; only accepted for loopback hosts.
    None,
}

impl TlsMode {
    pub fn default_port(self) -> u16 {
        match self {
            TlsMode::Implicit => 993,
            TlsMode::StartTls | TlsMode::None => 143,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            TlsMode::Implicit => "implicit",
            TlsMode::StartTls => "starttls",
            TlsMode::None => "none",
        }
    }
}

impl FromStr for TlsMode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "implicit" => Ok(TlsMode::Implicit),
            "starttls" => Ok(TlsMode::StartTls),
            "none" => Ok(TlsMode::None),
            other => bail!("unknown TLS mode {other:?} (expected implicit, starttls or none)"),
        }
    }
}

/// Connection settings for one mailbox. The password is never logged or serialized.
#[derive(Clone)]
pub struct ImapConfig {
    pub host: String,
    pub port: u16,
    pub tls: TlsMode,
    pub user: String,
    pub password: String,
    pub mailbox: String,
    /// Extra IMAP SEARCH criteria ANDed with the UID range, e.g. `FROM "scanner@example.com"`.
    pub search: Option<String>,
    /// PEM bundle trusted in addition to the operating system's roots.
    pub ca_file: Option<PathBuf>,
    /// Socket read/write timeout outside IDLE.
    pub io_timeout: Duration,
}

impl std::fmt::Debug for ImapConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImapConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .field("mailbox", &self.mailbox)
            .field("search", &self.search)
            .field("ca_file", &self.ca_file)
            .finish()
    }
}

impl ImapConfig {
    /// Reject settings that would send credentials in clear text over a network.
    pub fn validate(&self) -> Result<()> {
        if self.host.trim().is_empty() {
            bail!("IMAP host is empty");
        }
        if self.user.is_empty() {
            bail!("IMAP user is empty");
        }
        if self.password.is_empty() {
            bail!("IMAP password is empty");
        }
        if self.mailbox.is_empty() {
            bail!("IMAP mailbox is empty");
        }
        if self.tls == TlsMode::None && !is_loopback_host(&self.host) {
            bail!(
                "--tls none is only allowed for loopback hosts; {} would receive the password in clear text",
                self.host
            );
        }
        if let Some(search) = &self.search
            && search.contains(['\r', '\n'])
        {
            bail!("IMAP search criteria must be a single line");
        }
        Ok(())
    }
}

pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Read a password from a file, dropping one trailing line ending.
pub fn read_password(path: &std::path::Path) -> Result<String> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read password file {}", path.display()))?;
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let text = text.strip_suffix('\r').unwrap_or(text);
    Ok(text.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(host: &str, tls: TlsMode) -> ImapConfig {
        ImapConfig {
            host: host.into(),
            port: tls.default_port(),
            tls,
            user: "u".into(),
            password: "p".into(),
            mailbox: "INBOX".into(),
            search: None,
            ca_file: None,
            io_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn plaintext_is_refused_for_remote_hosts() {
        assert!(
            config("imap.example.com", TlsMode::None)
                .validate()
                .is_err()
        );
        assert!(config("127.0.0.1", TlsMode::None).validate().is_ok());
        assert!(config("[::1]", TlsMode::None).validate().is_ok());
        assert!(config("LOCALHOST", TlsMode::None).validate().is_ok());
        assert!(
            config("imap.example.com", TlsMode::Implicit)
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn multiline_search_criteria_are_rejected() {
        let mut c = config("imap.example.com", TlsMode::Implicit);
        c.search = Some("ALL\r\nA1 LOGOUT".into());
        assert!(c.validate().is_err());
    }

    #[test]
    fn debug_output_redacts_the_password() {
        let mut c = config("imap.example.com", TlsMode::Implicit);
        c.password = "hunter2".into();
        assert!(!format!("{c:?}").contains("hunter2"));
    }

    #[test]
    fn password_file_drops_one_trailing_line_ending() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pw");
        std::fs::write(&path, "s3cret \r\n").unwrap();
        assert_eq!(read_password(&path).unwrap(), "s3cret ");
    }
}
