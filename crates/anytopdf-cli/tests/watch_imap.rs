use std::process::Output;

#[path = "common/process.rs"]
mod process;
use process::command;
#[path = "common/output.rs"]
mod output;
use output::stderr;

fn watch(args: &[&str], password: Option<&str>) -> Output {
    let mut cmd = command();
    cmd.args(["watch", "imap"]).args(args);
    cmd.env_remove("ANYTOPDF_IMAP_PASSWORD");
    if let Some(password) = password {
        cmd.env("ANYTOPDF_IMAP_PASSWORD", password);
    }
    cmd.output().unwrap()
}

#[cfg(not(feature = "imap"))]
#[test]
fn watch_imap_explains_missing_feature() {
    let dir = tempfile::tempdir().unwrap();
    let out = watch(
        &[
            "--host",
            "localhost",
            "--user",
            "u",
            "--output-dir",
            dir.path().to_str().unwrap(),
        ],
        Some("p"),
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("--features imap"), "{}", stderr(&out));
}

#[cfg(feature = "imap")]
#[path = "../../anytopdf-imap/tests/support/fake_server.rs"]
mod fake_server;

#[cfg(feature = "imap")]
mod with_imap {
    use super::*;

    use super::fake_server::{PASSWORD, Shared, USER, serve};

    fn args<'a>(port: &'a str, out: &'a str) -> Vec<&'a str> {
        vec![
            "--host",
            "127.0.0.1",
            "--port",
            port,
            "--tls",
            "none",
            "--user",
            USER,
            "--output-dir",
            out,
            "--once",
        ]
    }

    #[test]
    fn once_converts_each_new_message_to_its_own_pdf() {
        let shared = Shared::default();
        shared.lock().unwrap().messages.insert(
            3,
            b"From: scanner@example.com\r\nSubject: Invoice\r\n\r\nWatcher acceptance marker\r\n"
                .to_vec(),
        );
        let port = serve(shared.clone()).to_string();
        let dir = tempfile::tempdir().unwrap();
        let out_dir = dir.path().to_str().unwrap();
        let mut first = args(&port, out_dir);
        let graph = dir.path().join("graph.json");
        first.extend(["--backfill", "--", "--ocr", "off", "--dump-graph"]);
        first.push(graph.to_str().unwrap());
        let out = watch(&first, Some(PASSWORD));
        assert!(out.status.success(), "{}", stderr(&out));
        let pdf = dir.path().join("INBOX-42-3.pdf");
        assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF-"));
        assert!(stderr(&out).contains("1 converted"), "{}", stderr(&out));
        // The built-in email importer, not the plain-text fallback, renders the message.
        let graph: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&graph).unwrap()).unwrap();
        assert_eq!(graph["sources"][0]["metadata"]["email.subject"], "Invoice");
        assert!(
            graph["units"][0]["visible_text"]
                .as_str()
                .unwrap()
                .contains("Watcher acceptance marker")
        );

        let again = watch(&args(&port, out_dir), Some(PASSWORD));
        assert!(again.status.success(), "{}", stderr(&again));
        assert!(stderr(&again).contains("0 converted"), "{}", stderr(&again));
        assert!(
            !dir.path()
                .join(".anytopdf-imap/spool/INBOX-42-3.eml")
                .exists(),
            "delivered messages leave the spool"
        );
    }

    #[test]
    fn missing_password_is_a_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let out = watch(&args("1", dir.path().to_str().unwrap()), None);
        assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
        assert!(stderr(&out).contains("ANYTOPDF_IMAP_PASSWORD"));
    }

    #[test]
    fn plaintext_to_a_remote_host_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let out = watch(
            &[
                "--host",
                "imap.example.com",
                "--tls",
                "none",
                "--user",
                "u",
                "--output-dir",
                dir.path().to_str().unwrap(),
            ],
            Some("p"),
        );
        assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
        assert!(stderr(&out).contains("clear text"), "{}", stderr(&out));
    }

    #[test]
    fn invalid_convert_options_are_rejected_before_connecting() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = args("1", dir.path().to_str().unwrap());
        a.extend(["--", "-o", "elsewhere.pdf"]);
        let out = watch(&a, Some("p"));
        assert_eq!(out.status.code(), Some(2), "{}", stderr(&out));
        assert!(stderr(&out).contains("after `--`"), "{}", stderr(&out));
    }

    #[test]
    fn unreachable_server_is_a_provider_error() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port().to_string();
        drop(listener);
        let dir = tempfile::tempdir().unwrap();
        let out = watch(&args(&port, dir.path().to_str().unwrap()), Some("p"));
        assert_eq!(out.status.code(), Some(4), "{}", stderr(&out));
    }
}
