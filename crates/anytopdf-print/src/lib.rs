//! Remote printing for anytopdf (backlog item 4).
//!
//! The IPP print helper (backlog item 3) listens on localhost only. This crate
//! adds what it takes to reach it from another network safely: a TLS front
//! that checks a peer allowlist and an HTTP Basic password before passing the
//! connection through, and DNS-SD descriptions for discovery over unicast DNS
//! (remote clients) and multicast DNS (the local network).

mod allow;
mod auth;
mod dnssd;
mod front;
mod guard;
mod ipp;
mod mdns;
mod receipts;

pub use allow::{Allowlist, Cidr, TAILNET_RANGES};
pub use auth::{USERS_SCHEMA, Users, hash_password, parse_basic};
pub use dnssd::{RESOURCE, SERVICE, SUBTYPES, ServiceSpec};
pub use front::{Front, FrontConfig, load_tls};
pub use guard::{Exposure, GuardError, check};
pub use ipp::IppSummary;
pub use mdns::{Advertisement, advertise, service_info};
pub use receipts::{RECEIPT_SCHEMA, Receipts};

/// Default port for both the loopback print helper and the remote front; above
/// 1024 so neither needs elevated privileges.
pub const DEFAULT_PORT: u16 = 8631;
