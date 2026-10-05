use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::mpsc;

#[path = "common/process.rs"]
mod process;
#[path = "common/schema_assert.rs"]
mod schema_assert;
#[path = "common/schema.rs"]
mod schema_files;

use process::command;
use schema_assert::assert_valid;

/// A fixed signing secret: `whsec_` + base64 of 32 bytes.
const SECRET: &str = "whsec_MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";

fn queue(args: &[&str], cwd: &Path, secret: bool) -> Output {
    let mut cmd = command();
    cmd.arg("queue").args(args).current_dir(cwd);
    if secret {
        cmd.env("ANYTOPDF_WEBHOOK_SECRET", SECRET);
    } else {
        cmd.env_remove("ANYTOPDF_WEBHOOK_SECRET");
    }
    cmd.output().expect("run anytopdf queue")
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn jobs(root: &Path, state: &str) -> Vec<Value> {
    let mut paths: Vec<PathBuf> = fs::read_dir(root.join("jobs").join(state))
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| serde_json::from_slice(&fs::read(p).unwrap()).unwrap())
        .collect()
}

struct Request {
    headers: HashMap<String, String>,
    body: String,
}

/// Answer every request with `status`, closing each connection, and forward what arrived.
fn receiver(status: u16) -> (String, mpsc::Receiver<Request>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/hook", listener.local_addr().unwrap());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut headers = HashMap::new();
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            loop {
                line.clear();
                reader.read_line(&mut line).unwrap();
                let Some((name, value)) = line.trim_end().split_once(':') else {
                    break;
                };
                headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
            }
            let length = headers["content-length"].parse::<usize>().unwrap();
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
            let _ = tx.send(Request {
                headers,
                body: String::from_utf8(body).unwrap(),
            });
        }
    });
    (url, rx)
}

fn expected_signature(request: &Request) -> String {
    let key = STANDARD
        .decode(SECRET.trim_start_matches("whsec_"))
        .unwrap();
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key).unwrap();
    mac.update(
        format!(
            "{}.{}.{}",
            request.headers["webhook-id"], request.headers["webhook-timestamp"], request.body
        )
        .as_bytes(),
    );
    format!("v1,{}", STANDARD.encode(mac.finalize().into_bytes()))
}

#[test]
fn once_worker_converts_added_jobs_and_inbox_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::write(dir.join("notes.txt"), "queued notes\n").unwrap();
    let id = ok(&queue(
        &["add", "q", "notes.txt", "--", "--ocr", "off"],
        dir,
        false,
    ));
    assert!(id.starts_with("job_"), "{id}");
    fs::write(dir.join("q/inbox/dropped.txt"), "dropped into the inbox\n").unwrap();
    fs::write(dir.join("q/inbox/still-copying.txt.part"), "partial").unwrap();
    fs::write(dir.join("q/inbox/.hidden.txt"), "hidden").unwrap();

    ok(&queue(
        &[
            "work",
            "q",
            "--once",
            "--poll-interval",
            "0.1",
            "--",
            "--ocr",
            "off",
        ],
        dir,
        false,
    ));

    let root = dir.join("q");
    let done = jobs(&root, "done");
    assert_eq!(done.len(), 2, "{done:#?}");
    assert!(jobs(&root, "pending").is_empty());
    assert!(jobs(&root, "running").is_empty());
    assert!(jobs(&root, "failed").is_empty());
    for job in &done {
        assert_valid("job", job);
        assert_eq!(job["state"], "succeeded");
        assert_eq!(job["exit_code"], 0);
        let output = PathBuf::from(job["output"].as_str().unwrap());
        assert!(fs::read(&output).unwrap().starts_with(b"%PDF-"));
        let work = root.join("work").join(job["id"].as_str().unwrap());
        let events = fs::read_to_string(work.join("events.ndjson")).unwrap();
        let last: Value = serde_json::from_str(events.lines().last().unwrap()).unwrap();
        assert_eq!(last["event"], "run.finished");
        assert_eq!(last["status"], "ok");
        let report: Value =
            serde_json::from_slice(&fs::read(work.join("convert.json")).unwrap()).unwrap();
        assert_eq!(report["schema_version"], "anytopdf.convert/1");
    }
    assert_eq!(done[0]["id"], id.as_str());
    assert_eq!(done[0]["origin"], "cli");
    assert_eq!(done[1]["origin"], "inbox");
    assert!(root.join("outbox/notes.pdf").is_file());
    assert!(root.join("outbox/dropped.pdf").is_file());
    assert!(root.join("inbox/still-copying.txt.part").is_file());
    assert!(root.join("inbox/.hidden.txt").is_file());
    assert!(!root.join("inbox/dropped.txt").exists());

    let status = ok(&queue(&["status", "q"], dir, false));
    assert!(status.contains(&format!("{id}  succeeded")), "{status}");
    assert!(status.contains("-> outbox/dropped.pdf"), "{status}");
}

#[test]
fn queue_options_are_validated_before_anything_runs() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::write(dir.join("a.txt"), "a").unwrap();
    for args in [
        &["add", "q", "a.txt", "--", "-o", "x.pdf"][..],
        &["add", "q", "a.txt", "--", "--events"],
        &["add", "q", "a.txt", "--", "--no-such-option"],
        &[
            "work",
            "q",
            "--once",
            "--webhook",
            "http://127.0.0.1:9/hook",
        ],
        &["work", "q", "--once", "--poll-interval", "0"],
    ] {
        let out = queue(args, dir, false);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
    }
    let missing = queue(&["add", "q", "missing.txt"], dir, false);
    assert_eq!(missing.status.code(), Some(3));
    assert!(!dir.join("q/jobs/pending").exists() || jobs(&dir.join("q"), "pending").is_empty());
}

#[test]
fn webhooks_are_signed_path_free_and_follow_each_job() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::write(dir.join("good.txt"), "good\n").unwrap();
    fs::write(dir.join("blob.xyz"), [0u8, 159, 146, 150, 7, 3]).unwrap();
    let good = ok(&queue(
        &["add", "q", "good.txt", "--", "--ocr", "off"],
        dir,
        false,
    ));
    let bad = ok(&queue(&["add", "q", "blob.xyz"], dir, false));
    let (url, rx) = receiver(200);

    ok(&queue(
        &[
            "work",
            "q",
            "--once",
            "--poll-interval",
            "0.1",
            "--webhook",
            &url,
        ],
        dir,
        true,
    ));

    let requests: Vec<Request> = rx.try_iter().collect();
    let seen: Vec<(String, String)> = requests
        .iter()
        .map(|r| {
            let body: Value = serde_json::from_str(&r.body).unwrap();
            assert_valid("webhook", &body);
            (
                body["type"].as_str().unwrap().to_string(),
                body["data"]["job_id"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    let expected = [
        ("job.received", &good),
        ("job.completed", &good),
        ("job.received", &bad),
        ("job.failed", &bad),
    ]
    .map(|(t, id)| (t.to_string(), id.clone()));
    assert_eq!(seen, expected);

    let root = dir.canonicalize().unwrap();
    for request in &requests {
        assert_eq!(request.headers["content-type"], "application/json");
        assert!(request.headers["webhook-id"].starts_with("msg_"));
        assert_eq!(
            request.headers["webhook-signature"],
            expected_signature(request)
        );
        let escaped = serde_json::to_string(&root.display().to_string()).unwrap();
        assert!(
            !request.body.contains(escaped.trim_matches('"')),
            "absolute path in {}",
            request.body
        );
    }
    let completed: Value = serde_json::from_str(&requests[1].body).unwrap();
    assert_eq!(completed["data"]["output"], "outbox/good.pdf");
    assert_eq!(completed["data"]["inputs"][0], "good.txt");
    let failed: Value = serde_json::from_str(&requests[3].body).unwrap();
    assert_eq!(failed["data"]["exit_code"], 3);
    assert_eq!(jobs(&dir.join("q"), "failed")[0]["id"], bad.as_str());
    assert!(
        fs::read_dir(dir.join("q/webhooks/pending"))
            .unwrap()
            .next()
            .is_none()
    );
}

#[test]
fn rejected_webhooks_stay_in_the_outbox_for_retry() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    fs::write(dir.join("a.txt"), "a\n").unwrap();
    ok(&queue(
        &["add", "q", "a.txt", "--", "--ocr", "off"],
        dir,
        false,
    ));
    let (url, rx) = receiver(500);

    ok(&queue(
        &[
            "work",
            "q",
            "--once",
            "--poll-interval",
            "0.1",
            "--webhook",
            &url,
        ],
        dir,
        true,
    ));

    assert!(rx.try_iter().count() >= 2);
    let pending: Vec<Value> = fs::read_dir(dir.join("q/webhooks/pending"))
        .unwrap()
        .map(|e| serde_json::from_slice(&fs::read(e.unwrap().path()).unwrap()).unwrap())
        .collect();
    assert_eq!(pending.len(), 2, "{pending:#?}");
    for delivery in &pending {
        assert!(delivery["attempts"].as_u64().unwrap() >= 1);
        assert_eq!(delivery["last_result"], "HTTP 500");
    }
    let status = ok(&queue(&["status", "q"], dir, false));
    assert!(status.contains("webhooks: 2 pending, 0 failed"), "{status}");
}
