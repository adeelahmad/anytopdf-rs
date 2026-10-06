//! Refuse URLs that point into the local machine or network unless the user opts in.

use anyhow::{Result, bail};
use std::net::{IpAddr, ToSocketAddrs};
use ureq::http::Uri;

/// Fail when `uri` is not http(s) or, without `allow_private`, names or resolves to a
/// loopback, private, link-local or otherwise non-public address.
///
/// A host that does not resolve here (for example behind an HTTP proxy) is allowed;
/// the fetch itself then fails or goes through the proxy.
pub(super) fn check(uri: &Uri, allow_private: bool) -> Result<()> {
    let scheme = uri.scheme_str().unwrap_or_default();
    if scheme != "http" && scheme != "https" {
        bail!("only http:// and https:// URLs can be fetched: {uri}");
    }
    let Some(host) = uri.host() else {
        bail!("URL has no host: {uri}");
    };
    if allow_private {
        return Ok(());
    }
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let lower = host.to_ascii_lowercase();
    if lower == "localhost" || lower.ends_with(".localhost") {
        bail!(refusal(host, "localhost"));
    }
    let port = uri
        .port_u16()
        .unwrap_or(if scheme == "https" { 443 } else { 80 });
    let addrs: Vec<IpAddr> = match host.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => (host, port)
            .to_socket_addrs()
            .map(|a| a.map(|s| s.ip()).collect())
            .unwrap_or_default(),
    };
    if let Some(ip) = addrs.iter().find(|ip| !is_public(ip)) {
        bail!(refusal(host, &ip.to_string()));
    }
    Ok(())
}

fn refusal(host: &str, addr: &str) -> String {
    format!(
        "refusing to fetch {host}: it is a local or private address ({addr}); \
         pass --url-allow-private to fetch it"
    )
}

pub(super) fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || o[0] >= 240
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
                || (o[0] == 192 && o[1] == 0 && o[2] == 0)
                || (o[0] == 198 && (o[1] & 0xfe) == 18))
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(&IpAddr::V4(v4));
            }
            let s = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00
                || (s[0] & 0xffc0) == 0xfe80
                || (s[0] == 0x2001 && s[1] == 0x0db8))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uri(s: &str) -> Uri {
        s.parse().unwrap()
    }

    #[test]
    fn private_and_loopback_addresses_are_not_public() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!is_public(&ip.parse().unwrap()), "{ip}");
        }
        for ip in ["93.184.215.14", "1.1.1.1", "2606:4700::1111"] {
            assert!(is_public(&ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn check_refuses_local_hosts_unless_allowed() {
        for u in [
            "http://127.0.0.1:8080/x",
            "http://localhost/",
            "http://[::1]/",
            "http://169.254.169.254/latest/meta-data",
        ] {
            let err = check(&uri(u), false).unwrap_err().to_string();
            assert!(err.contains("--url-allow-private"), "{u}: {err}");
            check(&uri(u), true).unwrap();
        }
        assert!(check(&uri("ftp://example.com/x"), true).is_err());
        check(&uri("https://93.184.215.14/"), false).unwrap();
    }
}
