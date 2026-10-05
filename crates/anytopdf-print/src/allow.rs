//! Peer allowlist: connections from addresses outside every listed CIDR are
//! closed before any TLS or IPP bytes are read.

use anyhow::{Context, Result, bail};
use std::fmt;
use std::net::IpAddr;
use std::str::FromStr;

/// Tailscale's IPv4 CGNAT range and IPv6 ULA prefix, the suggested default.
pub const TAILNET_RANGES: [&str; 2] = ["100.64.0.0/10", "fd7a:115c:a1e0::/48"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

impl Cidr {
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, canonical(ip)) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                masked(u32::from(net).into(), self.prefix, 32)
                    == masked(u32::from(ip).into(), self.prefix, 32)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                masked(u128::from(net), self.prefix, 128)
                    == masked(u128::from(ip), self.prefix, 128)
            }
            _ => false,
        }
    }

    /// True for `0.0.0.0/0` and `::/0`, which admit every peer.
    pub fn is_everything(&self) -> bool {
        self.prefix == 0
    }
}

fn masked(bits: u128, prefix: u8, width: u8) -> u128 {
    if prefix == 0 {
        0
    } else {
        bits >> (width - prefix)
    }
}

/// Maps IPv4-mapped IPv6 peers (`::ffff:a.b.c.d`) back to IPv4 so a dual-stack
/// listener matches IPv4 rules.
fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        v4 => v4,
    }
}

impl FromStr for Cidr {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (text, None),
        };
        let addr: IpAddr = addr
            .parse()
            .with_context(|| format!("invalid address in CIDR {text:?}"))?;
        let width = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            Some(p) => p
                .parse::<u8>()
                .with_context(|| format!("invalid prefix length in CIDR {text:?}"))?,
            None => width,
        };
        if prefix > width {
            bail!("prefix length /{prefix} is too long for {addr} in CIDR {text:?}");
        }
        Ok(Cidr { addr, prefix })
    }
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Allowlist(Vec<Cidr>);

impl Allowlist {
    pub fn new(cidrs: Vec<Cidr>) -> Self {
        Allowlist(cidrs)
    }

    pub fn parse<S: AsRef<str>>(items: &[S]) -> Result<Self> {
        items
            .iter()
            .map(|s| s.as_ref().parse())
            .collect::<Result<Vec<_>>>()
            .map(Allowlist)
    }

    pub fn permits(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|c| c.contains(ip))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn admits_everyone(&self) -> bool {
        self.0.iter().any(Cidr::is_everything)
    }

    pub fn cidrs(&self) -> &[Cidr] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn tailnet_ranges_admit_tailscale_peers_only() {
        let list = Allowlist::parse(&TAILNET_RANGES).unwrap();
        assert!(list.permits(ip("100.101.102.103")));
        assert!(list.permits(ip("fd7a:115c:a1e0::1234")));
        assert!(!list.permits(ip("100.128.0.1")));
        assert!(!list.permits(ip("192.168.1.10")));
        assert!(!list.permits(ip("fd7a:115c:a1e1::1")));
    }

    #[test]
    fn ipv4_mapped_ipv6_peers_match_ipv4_rules() {
        let list = Allowlist::parse(&["10.0.0.0/8"]).unwrap();
        assert!(list.permits(ip("::ffff:10.1.2.3")));
    }

    #[test]
    fn bare_address_is_a_single_host() {
        let cidr: Cidr = "192.0.2.7".parse().unwrap();
        assert_eq!(cidr.to_string(), "192.0.2.7/32");
        assert!(cidr.contains(ip("192.0.2.7")));
        assert!(!cidr.contains(ip("192.0.2.8")));
    }

    #[test]
    fn zero_prefix_admits_everyone() {
        let list = Allowlist::parse(&["0.0.0.0/0"]).unwrap();
        assert!(list.admits_everyone());
        assert!(list.permits(ip("203.0.113.1")));
        assert!(!list.permits(ip("2001:db8::1")));
    }

    #[test]
    fn malformed_cidrs_are_rejected() {
        for bad in ["10.0.0.0/33", "::/129", "nope/8", "10.0.0.0/x", ""] {
            assert!(bad.parse::<Cidr>().is_err(), "{bad} parsed");
        }
    }
}
