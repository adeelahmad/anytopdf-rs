//! TLS paths against the scripted server with a throwaway certificate authority.
use anytopdf_imap::{Credential, ImapConfig, Mailbox, TlsMode, connect};
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use std::{net::TcpStream, path::Path, sync::Arc, time::Duration};

#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::{PASSWORD, Security, Shared, Stream, USER, Upgrade, serve_with};

/// Returns the CA certificate in PEM and a server upgrade for `localhost`.
fn authority() -> (String, Upgrade) {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();
    let key = KeyPair::generate().unwrap();
    let issuer = rcgen::Issuer::new(ca_params, ca_key);
    let cert = CertificateParams::new(vec!["localhost".to_string()])
        .unwrap()
        .signed_by(&key, &issuer)
        .unwrap();
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = Arc::new(
        rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
            )
            .unwrap(),
    );
    let upgrade: Upgrade = Arc::new(move |tcp: TcpStream| -> Box<dyn Stream> {
        let conn = rustls::ServerConnection::new(config.clone()).unwrap();
        Box::new(rustls::StreamOwned::new(conn, tcp))
    });
    (ca.pem(), upgrade)
}

fn config(port: u16, tls: TlsMode, ca_file: Option<&Path>) -> ImapConfig {
    ImapConfig {
        host: "localhost".into(),
        port,
        tls,
        user: USER.into(),
        credential: Credential::Password(PASSWORD.into()),
        mailbox: "INBOX".into(),
        search: None,
        ca_file: ca_file.map(Path::to_path_buf),
        io_timeout: Duration::from_secs(5),
    }
}

#[test]
fn implicit_tls_trusts_a_ca_file() {
    let (ca, upgrade) = authority();
    let dir = tempfile::tempdir().unwrap();
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&ca_file, ca).unwrap();
    let port = serve_with(Shared::default(), Security::Implicit(upgrade));
    let mut mailbox = connect(&config(port, TlsMode::Implicit, Some(&ca_file))).unwrap();
    assert_eq!(mailbox.status().unwrap().uid_validity, 42);
}

#[test]
fn starttls_upgrades_before_login() {
    let (ca, upgrade) = authority();
    let dir = tempfile::tempdir().unwrap();
    let ca_file = dir.path().join("ca.pem");
    std::fs::write(&ca_file, ca).unwrap();
    let shared = Shared::default();
    let port = serve_with(shared.clone(), Security::StartTls(upgrade));
    let mut mailbox = connect(&config(port, TlsMode::StartTls, Some(&ca_file))).unwrap();
    assert_eq!(mailbox.status().unwrap().uid_validity, 42);
    let commands = shared.lock().unwrap().commands.clone();
    assert_eq!(commands[0], "STARTTLS");
    assert!(commands[1].starts_with("LOGIN"));
}

#[test]
fn untrusted_certificate_is_rejected_before_login() {
    let (_ca, upgrade) = authority();
    let shared = Shared::default();
    let port = serve_with(shared.clone(), Security::Implicit(upgrade));
    // Fails in the handshake, or earlier on hosts with an empty trust store.
    assert!(connect(&config(port, TlsMode::Implicit, None)).is_err());
    assert!(shared.lock().unwrap().commands.is_empty());
}
