//! Drives the real IMAP transport against a small scripted server on loopback.
use anyhow::Result;
use anytopdf_imap::{
    Credential, Delivery, ImapConfig, MessageSink, SpooledMessage, TlsMode, WatchOptions, Watcher,
    connect,
};
use std::time::Duration;

#[path = "support/fake_server.rs"]
mod fake_server;
use fake_server::{PASSWORD, Shared, TOKEN, USER, serve};

fn config(port: u16, tls: TlsMode) -> ImapConfig {
    ImapConfig {
        host: "127.0.0.1".into(),
        port,
        tls,
        user: USER.into(),
        credential: Credential::Password(PASSWORD.into()),
        mailbox: "INBOX".into(),
        search: None,
        ca_file: None,
        io_timeout: Duration::from_secs(5),
    }
}

#[derive(Default)]
struct Recorder(Vec<(u32, Vec<u8>)>);

impl MessageSink for Recorder {
    fn deliver(&mut self, message: &SpooledMessage) -> Result<Delivery> {
        self.0.push((message.uid, std::fs::read(&message.path)?));
        Ok(Delivery::default())
    }
}

fn message(subject: &str) -> Vec<u8> {
    format!("From: a@example.com\r\nSubject: {subject}\r\n\r\nhello\r\n").into_bytes()
}

#[test]
fn watcher_fetches_new_messages_over_imap_once_each() {
    let shared = Shared::default();
    shared.lock().unwrap().messages.insert(3, message("three"));
    shared.lock().unwrap().messages.insert(8, message("eight"));
    let port = serve(shared.clone());
    let dir = tempfile::tempdir().unwrap();
    let watcher = Watcher::new(
        "127.0.0.1",
        "user",
        "INBOX",
        WatchOptions {
            state_dir: dir.path().to_path_buf(),
            backfill: true,
            max_message_bytes: 1 << 20,
            max_attempts: 3,
            mark_seen: true,
            move_to: None,
            keep_eml: false,
            senders: Default::default(),
            quiet: true,
        },
    );
    let mut sink = Recorder::default();
    let mut mailbox = connect(&config(port, TlsMode::None)).unwrap();
    let report = watcher.poll_once(&mut mailbox, &mut sink).unwrap();
    assert_eq!(report.delivered, vec![3, 8]);
    assert_eq!(sink.0[0].1, message("three"));

    // `UID 9:*` still matches UID 8 on the server; it must not be delivered twice.
    let again = watcher.poll_once(&mut mailbox, &mut sink).unwrap();
    assert!(again.delivered.is_empty());
    drop(mailbox);

    let commands = shared.lock().unwrap().commands.clone();
    assert!(commands.iter().any(|c| c == "UID FETCH 3 BODY.PEEK[]"));
    assert!(
        commands
            .iter()
            .any(|c| c.starts_with("UID STORE 8 +FLAGS.SILENT (\\Seen)"))
    );
    assert!(
        !commands
            .iter()
            .any(|c| c.contains("BODY[]") && !c.contains("PEEK"))
    );
}

#[test]
fn wrong_password_is_a_login_error() {
    let port = serve(Shared::default());
    let mut cfg = config(port, TlsMode::None);
    cfg.credential = Credential::Password("nope".into());
    let err = connect(&cfg).err().unwrap();
    assert!(format!("{err:#}").contains("IMAP login as user"), "{err:#}");
}

#[test]
fn starttls_refusal_stops_before_sending_credentials() {
    let shared = Shared::default();
    let port = serve(shared.clone());
    assert!(connect(&config(port, TlsMode::StartTls)).is_err());
    let commands = shared.lock().unwrap().commands.clone();
    assert_eq!(commands, vec!["STARTTLS".to_string()]);
}

#[test]
fn xoauth2_logs_in_with_a_token_and_rejects_a_bad_one() {
    let shared = Shared::default();
    let port = serve(shared.clone());
    let mut cfg = config(port, TlsMode::None);
    cfg.credential = Credential::OAuth2Token(TOKEN.into());
    let mut mailbox = connect(&cfg).unwrap();
    use anytopdf_imap::Mailbox;
    assert_eq!(mailbox.status().unwrap().uid_validity, 42);
    drop(mailbox);
    assert!(
        shared
            .lock()
            .unwrap()
            .commands
            .iter()
            .all(|c| !c.starts_with("LOGIN"))
    );

    cfg.credential = Credential::OAuth2Token("expired".into());
    let err = connect(&cfg).err().unwrap();
    assert!(format!("{err:#}").contains("IMAP login as user"), "{err:#}");
}

#[test]
fn xoauth2_token_file_is_read_on_each_connection() {
    let port = serve(Shared::default());
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("token");
    std::fs::write(&file, "stale\n").unwrap();
    let mut cfg = config(port, TlsMode::None);
    cfg.credential = Credential::OAuth2TokenFile(file.clone());
    assert!(connect(&cfg).is_err());
    std::fs::write(&file, format!("{TOKEN}\n")).unwrap();
    assert!(connect(&cfg).is_ok());
}
