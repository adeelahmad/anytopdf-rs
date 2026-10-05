//! LAN discovery: advertises the printer over multicast DNS so AirPrint and
//! IPP Everywhere clients on the same network find it without DNS setup.
//! Multicast does not cross WireGuard or Tailscale; remote clients use the
//! unicast records from [`crate::ServiceSpec::zone`].

use crate::dnssd::{AIRPRINT_SUBTYPE, SERVICE, ServiceSpec};
use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use std::collections::HashMap;
use std::net::IpAddr;

/// A running advertisement; dropping it withdraws the records.
pub struct Advertisement {
    daemon: ServiceDaemon,
    fullname: String,
}

/// Builds the mDNS record. mdns-sd answers both the `_ipps._tcp` type (IPP
/// Everywhere browsers) and one subtype per instance, so the subtype is the
/// AirPrint `_universal`. `host` must be a `.local` name; with no addresses the
/// responder uses every interface.
pub fn service_info(spec: &ServiceSpec, host: &str, addresses: &[IpAddr]) -> Result<ServiceInfo> {
    let host = format!("{}.", host.trim_end_matches('.'));
    let properties: HashMap<String, String> = spec.txt().into_iter().collect();
    let ty = format!("{AIRPRINT_SUBTYPE}._sub.{SERVICE}.local.");
    let info = ServiceInfo::new(&ty, &spec.name, &host, addresses, spec.port, properties)
        .with_context(|| format!("cannot build mDNS record for {ty}"))?;
    Ok(if addresses.is_empty() {
        info.enable_addr_auto()
    } else {
        info
    })
}

pub fn advertise(spec: &ServiceSpec, host: &str, addresses: &[IpAddr]) -> Result<Advertisement> {
    let daemon = ServiceDaemon::new().context("cannot start the mDNS responder")?;
    let info = service_info(spec, host, addresses)?;
    let fullname = info.get_fullname().to_string();
    daemon
        .register(info)
        .context("cannot register the mDNS service")?;
    Ok(Advertisement { daemon, fullname })
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_is_an_airprint_subtype_of_ipps_with_the_txt_keys() {
        let spec = ServiceSpec::new("anytopdf", "printer.local", 8631);
        let info = service_info(&spec, "printer.local", &["192.0.2.10".parse().unwrap()]).unwrap();
        assert_eq!(info.get_fullname(), "anytopdf._ipps._tcp.local.");
        assert_eq!(info.get_type(), "_ipps._tcp.local.");
        assert_eq!(
            info.get_subtype().as_deref(),
            Some("_universal._sub._ipps._tcp.local.")
        );
        assert_eq!(info.get_port(), 8631);
        assert_eq!(info.get_property_val_str("rp"), Some("ipp/print"));
        assert_eq!(info.get_property_val_str("air"), Some("username,password"));
    }
}
