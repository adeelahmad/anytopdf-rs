//! Sender checks on raw message headers. This reads only the header block; it is
//! not a MIME parser.
use anyhow::{Result, bail};

/// Which senders may have their mail converted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SenderPolicy {
    /// Lowercase addresses (`a@example.com`) or domains (`@example.com`).
    allow: Vec<String>,
    /// Require `dmarc=pass` in the `Authentication-Results` header added by this
    /// receiving server (its authserv-id, e.g. `mx.google.com`).
    require_dmarc: Option<String>,
}

impl SenderPolicy {
    /// `allow` entries are addresses or domains (`example.com` or `@example.com`).
    pub fn new(allow: &[String], require_dmarc: Option<&str>) -> Result<Self> {
        let mut entries = Vec::new();
        for entry in allow {
            let entry = entry.trim().to_ascii_lowercase();
            let valid = match entry.split_once('@') {
                Some((local, domain)) => {
                    !domain.is_empty()
                        && !domain.contains('@')
                        && (local.is_empty() || !local.contains(char::is_whitespace))
                }
                None => !entry.is_empty() && entry.contains('.'),
            };
            if !valid || entry.contains(char::is_whitespace) {
                bail!("--allow-from {entry:?} is not an address or domain");
            }
            entries.push(if entry.contains('@') {
                entry
            } else {
                format!("@{entry}")
            });
        }
        let require_dmarc = require_dmarc.map(str::trim).filter(|s| !s.is_empty());
        if let Some(id) = require_dmarc
            && (id.contains(';') || id.contains(char::is_whitespace))
        {
            bail!("--require-dmarc {id:?} is not an authserv-id such as mx.google.com");
        }
        Ok(SenderPolicy {
            allow: entries,
            require_dmarc: require_dmarc.map(str::to_ascii_lowercase),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.allow.is_empty() && self.require_dmarc.is_none()
    }

    /// `Ok` when the message may be converted, otherwise the reason it may not.
    pub fn check(&self, raw: &[u8]) -> std::result::Result<(), String> {
        if self.is_empty() {
            return Ok(());
        }
        let headers = headers(raw);
        let from: Vec<&str> = headers
            .iter()
            .filter(|(name, _)| name == "from")
            .map(|(_, value)| value.as_str())
            .collect();
        let address = match from.as_slice() {
            [value] => address(value).ok_or("From header has no address")?,
            [] => return Err("message has no From header".into()),
            _ => return Err("message has more than one From header".into()),
        };
        if !self.allow.is_empty() && !self.allow.iter().any(|entry| allows(entry, &address)) {
            return Err(format!("sender {address} is not in --allow-from"));
        }
        if let Some(id) = &self.require_dmarc {
            // The receiving server prepends its result, so its header is the first one
            // carrying its authserv-id; any copies further down came from the sender.
            let result = headers
                .iter()
                .filter(|(name, _)| name == "authentication-results")
                .find(|(_, value)| authserv_id(value).eq_ignore_ascii_case(id))
                .map(|(_, value)| value.as_str())
                .ok_or_else(|| format!("no Authentication-Results from {id}"))?;
            let passed = result
                .split(';')
                .skip(1)
                .any(|part| part.trim().to_ascii_lowercase().starts_with("dmarc=pass"));
            if !passed {
                return Err(format!("{id} did not report dmarc=pass for {address}"));
            }
        }
        Ok(())
    }
}

fn allows(entry: &str, address: &str) -> bool {
    match entry.strip_prefix('@') {
        Some(domain) => address.rsplit_once('@').is_some_and(|(_, d)| d == domain),
        None => entry == address,
    }
}

/// The authserv-id: the first token before `;`, ignoring an optional version.
fn authserv_id(value: &str) -> &str {
    value
        .split(';')
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or_default()
}

/// Header fields up to the first blank line, names lowercased and values unfolded.
fn headers(raw: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(raw);
    let mut out: Vec<(String, String)> = Vec::new();
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            if let Some((_, value)) = out.last_mut() {
                value.push(' ');
                value.push_str(line.trim());
            }
            continue;
        }
        if let Some((name, value)) = line.split_once(':') {
            out.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    out
}

/// The addr-spec of a single-mailbox From value, lowercased.
fn address(value: &str) -> Option<String> {
    let value = strip_comments(value);
    let candidate = match (value.rfind('<'), value.rfind('>')) {
        (Some(start), Some(end)) if start < end => value[start + 1..end].trim().to_string(),
        _ => value.trim().to_string(),
    };
    let candidate = candidate.trim_matches('"').to_ascii_lowercase();
    let (local, domain) = candidate.rsplit_once('@')?;
    if local.is_empty()
        || domain.is_empty()
        || candidate.contains(char::is_whitespace)
        || candidate.contains(',')
    {
        return None;
    }
    Some(candidate)
}

fn strip_comments(value: &str) -> String {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut out = String::new();
    for c in value.chars() {
        match c {
            '"' if depth == 0 => {
                quoted = !quoted;
                out.push(c);
            }
            '(' if !quoted => depth += 1,
            ')' if !quoted && depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(allow: &[&str], dmarc: Option<&str>) -> SenderPolicy {
        let allow: Vec<String> = allow.iter().map(|s| s.to_string()).collect();
        SenderPolicy::new(&allow, dmarc).unwrap()
    }

    #[test]
    fn allowlist_matches_addresses_and_exact_domains() {
        let p = policy(&["Scanner@Example.com", "trusted.org"], None);
        let ok = |from: &str| p.check(format!("From: {from}\r\n\r\nbody").as_bytes());
        assert!(ok("\"Office Scanner\" <scanner@example.com>").is_ok());
        assert!(ok("someone@trusted.org").is_ok());
        assert!(ok("Mallory <mallory@example.com>").is_err());
        assert!(ok("x@evil-trusted.org").is_err());
        assert!(ok("x@sub.trusted.org").is_err());
        assert!(ok("a@trusted.org (comment <evil@x.com>)").is_ok());
    }

    #[test]
    fn missing_folded_or_duplicate_from_headers_are_handled() {
        let p = policy(&["@example.com"], None);
        assert!(p.check(b"From: Name\r\n <a@example.com>\r\n\r\n").is_ok());
        assert!(
            p.check(b"Subject: none\r\n\r\nFrom: a@example.com")
                .is_err()
        );
        assert!(
            p.check(b"From: a@example.com\r\nFrom: b@evil.com\r\n\r\n")
                .is_err()
        );
        assert!(p.check(b"From: a@example.com, b@evil.com\r\n\r\n").is_err());
    }

    #[test]
    fn dmarc_requires_the_receiving_servers_result() {
        let p = policy(&[], Some("mx.example.net"));
        let pass = b"Authentication-Results: mx.example.net;\r\n dkim=pass header.d=a.com;\r\n dmarc=pass header.from=a.com\r\nFrom: x@a.com\r\n\r\n";
        assert!(p.check(pass).is_ok());
        let fail = b"Authentication-Results: mx.example.net; dmarc=fail header.from=a.com\r\nFrom: x@a.com\r\n\r\n";
        assert!(p.check(fail).is_err());
        let forged_below = b"Authentication-Results: mx.example.net; dmarc=fail\r\nAuthentication-Results: mx.example.net; dmarc=pass\r\nFrom: x@a.com\r\n\r\n";
        assert!(p.check(forged_below).is_err());
        let other_server =
            b"Authentication-Results: mx.attacker.test; dmarc=pass\r\nFrom: x@a.com\r\n\r\n";
        assert!(p.check(other_server).is_err());
    }

    #[test]
    fn invalid_allow_entries_are_rejected() {
        for bad in ["", "nodomain", "a@", "a b@example.com", "a@b@c.com"] {
            assert!(
                SenderPolicy::new(&[bad.to_string()], None).is_err(),
                "{bad}"
            );
        }
        assert!(SenderPolicy::new(&[], Some("mx; evil")).is_err());
        assert!(SenderPolicy::new(&[], None).unwrap().is_empty());
    }
}
