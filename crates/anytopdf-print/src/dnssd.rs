//! DNS-SD description of the printer (RFC 6763, PWG 5100.14 TXT keys with the
//! AirPrint `_universal` subtype). The same description feeds the unicast DNS
//! zone snippet for remote clients and the mDNS advertisement on the LAN.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::net::IpAddr;

pub const SERVICE: &str = "_ipps._tcp";
/// AirPrint clients browse `_universal`; IPP Everywhere clients may browse `_print`.
pub const AIRPRINT_SUBTYPE: &str = "_universal";
pub const SUBTYPES: [&str; 2] = [AIRPRINT_SUBTYPE, "_print"];
/// The print helper's printer path (PAPPL names it after the printer).
pub const RESOURCE: &str = "ipp/print/anytopdf";

#[derive(Clone, Debug)]
pub struct ServiceSpec {
    /// Instance name shown to users, e.g. "anytopdf".
    pub name: String,
    /// Host name clients connect to, e.g. "printer.home.example".
    pub host: String,
    pub port: u16,
    /// Document formats the print helper accepts.
    pub formats: Vec<String>,
    pub color: bool,
    pub auth: bool,
    pub uuid: Option<uuid::Uuid>,
}

impl ServiceSpec {
    pub fn new(name: &str, host: &str, port: u16) -> Self {
        ServiceSpec {
            name: name.to_string(),
            host: host.trim_end_matches('.').to_string(),
            port,
            formats: vec!["image/pwg-raster".into(), "image/urf".into()],
            color: true,
            auth: true,
            uuid: None,
        }
    }

    /// A stable UUID derived from name, host and port unless one was given, so
    /// clients keep recognising the same printer across restarts.
    pub fn uuid(&self) -> uuid::Uuid {
        self.uuid.unwrap_or_else(|| {
            let digest = Sha256::digest(format!(
                "anytopdf-print\0{}\0{}\0{}",
                self.name, self.host, self.port
            ));
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&digest[..16]);
            uuid::Uuid::new_v8(bytes)
        })
    }

    pub fn url(&self) -> String {
        format!(
            "ipps://{}:{}/{RESOURCE}",
            host_for_url(&self.host),
            self.port
        )
    }

    /// TXT key/value pairs in a stable order.
    pub fn txt(&self) -> Vec<(String, String)> {
        let mut urf = vec!["W8", "CP1", "RS300"];
        if self.color {
            urf.insert(1, "SRGB24");
        }
        let mut pairs = vec![
            ("txtvers", "1".to_string()),
            ("qtotal", "1".into()),
            ("rp", RESOURCE.into()),
            ("ty", "anytopdf searchable PDF printer".into()),
            ("product", "(anytopdf)".into()),
            ("note", "Prints to a searchable PDF".into()),
            ("pdl", self.formats.join(",")),
            ("URF", urf.join(",")),
            ("Color", if self.color { "T" } else { "F" }.into()),
            ("Duplex", "F".into()),
            ("kind", "document".into()),
            ("PaperMax", "legal-A4".into()),
            ("TLS", "1.2".into()),
            ("UUID", self.uuid().hyphenated().to_string()),
        ];
        if self.auth {
            pairs.push(("air", "username,password".into()));
        }
        pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    /// Unicast DNS-SD records for `domain`, as a zone-file snippet. The host's
    /// address records are included only when `addresses` is non-empty.
    pub fn zone(&self, domain: &str, addresses: &[IpAddr]) -> String {
        let domain = domain.trim_end_matches('.');
        let service = format!("{SERVICE}.{domain}.");
        let instance = format!("{}.{service}", escape_label(&self.name));
        let mut out = vec![
            format!("; anytopdf remote printer, DNS-SD records for {domain}"),
            format!("; clients connect to {}", self.url()),
            format!("b._dns-sd._udp.{domain}.\tIN PTR\t{domain}."),
            format!("lb._dns-sd._udp.{domain}.\tIN PTR\t{domain}."),
            format!("_services._dns-sd._udp.{domain}.\tIN PTR\t{service}"),
            format!("{service}\tIN PTR\t{instance}"),
        ];
        for sub in SUBTYPES {
            out.push(format!("{sub}._sub.{service}\tIN PTR\t{instance}"));
        }
        out.push(format!(
            "{instance}\tIN SRV\t0 0 {} {}.",
            self.port, self.host
        ));
        let txt: Vec<String> = self
            .txt()
            .iter()
            .map(|(k, v)| format!("\"{}\"", escape_txt(&format!("{k}={v}"))))
            .collect();
        out.push(format!("{instance}\tIN TXT\t( {} )", txt.join(" ")));
        for addr in addresses {
            let kind = if addr.is_ipv4() { "A" } else { "AAAA" };
            out.push(format!("{}.\tIN {kind}\t{addr}", self.host));
        }
        out.join("\n") + "\n"
    }

    pub fn json(&self, domain: &str, addresses: &[IpAddr]) -> Value {
        let domain = domain.trim_end_matches('.');
        json!({
            "schema_version": "anytopdf.print-dns-sd/1",
            "domain": domain,
            "service": SERVICE,
            "subtypes": SUBTYPES,
            "instance": self.name,
            "host": self.host,
            "port": self.port,
            "url": self.url(),
            "txt": self.txt().into_iter().map(|(k, v)| json!({"key": k, "value": v})).collect::<Vec<_>>(),
            "addresses": addresses.iter().map(ToString::to_string).collect::<Vec<_>>(),
            "zone": self.zone(domain, addresses),
        })
    }
}

fn host_for_url(host: &str) -> String {
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V6(v6)) => format!("[{v6}]"),
        _ => host.to_string(),
    }
}

/// Escapes a DNS label for a zone file (RFC 1035 section 5.1).
fn escape_label(label: &str) -> String {
    let mut out = String::new();
    for byte in label.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' => out.push(byte as char),
            b'.' | b'\\' | b'"' | b'(' | b')' | b';' | b'@' | b'$' => {
                out.push('\\');
                out.push(byte as char);
            }
            _ => out.push_str(&format!("\\{byte:03}")),
        }
    }
    out
}

fn escape_txt(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ServiceSpec {
        ServiceSpec::new("Office PDF", "printer.home.example.", 8631)
    }

    #[test]
    fn txt_carries_ipp_everywhere_and_airprint_keys() {
        let txt = spec().txt();
        let get = |k: &str| {
            txt.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(get("rp"), Some("ipp/print/anytopdf"));
        assert_eq!(get("pdl"), Some("image/pwg-raster,image/urf"));
        assert_eq!(get("URF"), Some("W8,SRGB24,CP1,RS300"));
        assert_eq!(get("TLS"), Some("1.2"));
        assert_eq!(get("air"), Some("username,password"));
        assert!(txt.iter().all(|(k, v)| k.len() + v.len() < 255));
    }

    #[test]
    fn uuid_is_stable_for_the_same_printer() {
        assert_eq!(spec().uuid(), spec().uuid());
        assert_ne!(
            spec().uuid(),
            ServiceSpec::new("Other", "printer.home.example", 8631).uuid()
        );
    }

    #[test]
    fn zone_lists_browse_domain_subtypes_srv_and_addresses() {
        let zone = spec().zone(
            "home.example.",
            &[
                "100.101.102.103".parse().unwrap(),
                "fd7a:115c:a1e0::5".parse().unwrap(),
            ],
        );
        for line in [
            "b._dns-sd._udp.home.example.\tIN PTR\thome.example.",
            "_ipps._tcp.home.example.\tIN PTR\tOffice\\032PDF._ipps._tcp.home.example.",
            "_universal._sub._ipps._tcp.home.example.\tIN PTR\tOffice\\032PDF._ipps._tcp.home.example.",
            "_print._sub._ipps._tcp.home.example.\tIN PTR\tOffice\\032PDF._ipps._tcp.home.example.",
            "Office\\032PDF._ipps._tcp.home.example.\tIN SRV\t0 0 8631 printer.home.example.",
            "printer.home.example.\tIN A\t100.101.102.103",
            "printer.home.example.\tIN AAAA\tfd7a:115c:a1e0::5",
        ] {
            assert!(zone.contains(line), "missing {line:?} in\n{zone}");
        }
        assert!(zone.contains("\"rp=ipp/print/anytopdf\""));
    }

    #[test]
    fn url_brackets_ipv6_hosts() {
        assert_eq!(
            spec().url(),
            "ipps://printer.home.example:8631/ipp/print/anytopdf"
        );
        assert_eq!(
            ServiceSpec::new("p", "fd7a::1", 8631).url(),
            "ipps://[fd7a::1]:8631/ipp/print/anytopdf"
        );
    }

    #[test]
    fn json_embeds_the_zone_and_url() {
        let value = spec().json("home.example", &[]);
        assert_eq!(value["schema_version"], "anytopdf.print-dns-sd/1");
        assert_eq!(
            value["url"],
            "ipps://printer.home.example:8631/ipp/print/anytopdf"
        );
        assert!(value["zone"].as_str().unwrap().contains("IN SRV"));
    }
}
