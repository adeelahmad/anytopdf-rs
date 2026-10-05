//! Refuses unsafe remote-printing setups before any socket is opened.

use crate::allow::Allowlist;
use std::fmt;
use std::net::SocketAddr;

/// What the remote front is about to do, as far as safety checks care.
#[derive(Debug)]
pub struct Exposure<'a> {
    pub listen: SocketAddr,
    pub upstream: SocketAddr,
    pub has_users: bool,
    pub allow: &'a Allowlist,
    pub allow_public_bind: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum GuardError {
    UpstreamNotLoopback(SocketAddr),
    NoUsers,
    NoAllowlist,
    PublicBind(SocketAddr),
    AllowlistAdmitsEveryone,
}

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuardError::UpstreamNotLoopback(addr) => write!(
                f,
                "upstream {addr} is not a loopback address; the print helper must stay on localhost"
            ),
            GuardError::NoUsers => write!(
                f,
                "a non-loopback listener needs at least one print user; add one with `anytopdf print passwd`"
            ),
            GuardError::NoAllowlist => write!(
                f,
                "a non-loopback listener needs --allow CIDR or --allow-tailnet"
            ),
            GuardError::PublicBind(addr) => write!(
                f,
                "{addr} listens on every interface; bind the tailnet or WireGuard address, or pass --allow-public-bind"
            ),
            GuardError::AllowlistAdmitsEveryone => write!(
                f,
                "--allow 0.0.0.0/0 or ::/0 admits every peer; pass --allow-public-bind to accept that"
            ),
        }
    }
}

impl std::error::Error for GuardError {}

/// Every rule that applies, in a stable order; empty means the setup is accepted.
pub fn check(exposure: &Exposure<'_>) -> Vec<GuardError> {
    let mut errors = Vec::new();
    if !exposure.upstream.ip().is_loopback() {
        errors.push(GuardError::UpstreamNotLoopback(exposure.upstream));
    }
    if exposure.listen.ip().is_loopback() {
        return errors;
    }
    if !exposure.has_users {
        errors.push(GuardError::NoUsers);
    }
    if exposure.allow.is_empty() {
        errors.push(GuardError::NoAllowlist);
    }
    if !exposure.allow_public_bind {
        if exposure.listen.ip().is_unspecified() {
            errors.push(GuardError::PublicBind(exposure.listen));
        }
        if exposure.allow.admits_everyone() {
            errors.push(GuardError::AllowlistAdmitsEveryone);
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exposure<'a>(listen: &str, allow: &'a Allowlist, users: bool, public: bool) -> Exposure<'a> {
        Exposure {
            listen: listen.parse().unwrap(),
            upstream: "127.0.0.1:8631".parse().unwrap(),
            has_users: users,
            allow,
            allow_public_bind: public,
        }
    }

    #[test]
    fn tailnet_bind_with_users_and_allowlist_is_accepted() {
        let allow = Allowlist::parse(&crate::TAILNET_RANGES).unwrap();
        assert!(check(&exposure("100.101.102.103:8631", &allow, true, false)).is_empty());
    }

    #[test]
    fn loopback_bind_needs_nothing_else() {
        let allow = Allowlist::default();
        assert!(check(&exposure("127.0.0.1:8632", &allow, false, false)).is_empty());
    }

    #[test]
    fn remote_bind_without_users_or_allowlist_is_refused() {
        let allow = Allowlist::default();
        assert_eq!(
            check(&exposure("100.101.102.103:8631", &allow, false, false)),
            vec![GuardError::NoUsers, GuardError::NoAllowlist]
        );
    }

    #[test]
    fn wildcard_bind_and_open_allowlist_need_the_public_flag() {
        let allow = Allowlist::parse(&["0.0.0.0/0"]).unwrap();
        assert_eq!(
            check(&exposure("0.0.0.0:8631", &allow, true, false)),
            vec![
                GuardError::PublicBind("0.0.0.0:8631".parse().unwrap()),
                GuardError::AllowlistAdmitsEveryone
            ]
        );
        assert!(check(&exposure("0.0.0.0:8631", &allow, true, true)).is_empty());
    }

    #[test]
    fn non_loopback_upstream_is_always_refused() {
        let allow = Allowlist::default();
        let mut e = exposure("127.0.0.1:8632", &allow, false, true);
        e.upstream = "192.168.1.5:631".parse().unwrap();
        assert_eq!(check(&e), vec![GuardError::UpstreamNotLoopback(e.upstream)]);
    }
}
