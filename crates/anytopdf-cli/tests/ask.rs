use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
};

#[path = "common/process.rs"]
mod process;
#[path = "common/schema_assert.rs"]
mod schema_assert;
#[path = "common/schema.rs"]
mod schema_files;
#[path = "common/stdout_json.rs"]
mod stdout_json;
use schema_assert::assert_valid;
use stdout_json::single_document;

const LLM_VARS: [&str; 4] = [
    "ANYTOPDF_LLM_URL",
    "ANYTOPDF_LLM_MODEL",
    "ANYTOPDF_LLM_API_KEY",
    "ANYTOPDF_LLM_TIMEOUT",
];

fn anytopdf() -> Command {
    let mut cmd = process::command();
    cmd.env_remove("SOURCE_DATE_EPOCH");
    for var in LLM_VARS {
        cmd.env_remove(var);
    }
    cmd
}

/// Convert three small notes into one PDF inside `dir/pdfs`.
fn converted(dir: &Path) -> PathBuf {
    let notes = [
        (
            "roadmap.txt",
            "The roadmap meeting covered the spring release plan.",
        ),
        (
            "invoice.txt",
            "Invoice 42 from Acme is due on 3 March and totals 900 EUR.",
        ),
        ("lunch.txt", "Lunch is served at noon in the cafeteria."),
    ];
    let mut inputs = Vec::new();
    for (name, text) in notes {
        let path = dir.join(name);
        fs::write(&path, text).unwrap();
        inputs.push(path);
    }
    let out = dir.join("pdfs");
    fs::create_dir_all(&out).unwrap();
    let pdf = out.join("notes.pdf");
    let result = anytopdf()
        .arg("convert")
        .args(&inputs)
        .args(["--ocr", "off", "-o"])
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "convert failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    pdf
}

fn ask_json(cmd: &mut Command) -> (Value, Output) {
    let out = cmd.arg("--json").output().unwrap();
    assert!(
        out.status.success(),
        "ask failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = single_document(&out.stdout);
    assert_valid("ask", &doc);
    (doc, out)
}

#[test]
fn ask_without_llm_returns_ranked_passages_with_citations() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = converted(dir.path());
    let (doc, _) = ask_json(
        anytopdf()
            .arg("ask")
            .arg(&pdf)
            .arg("When is the Acme invoice due?"),
    );
    assert_eq!(doc["schema_version"], "anytopdf.ask/1");
    assert_eq!(doc["mode"], "retrieval");
    assert_eq!(doc["answer"], Value::Null);
    let first = &doc["passages"][0];
    assert_eq!(first["n"], 1);
    assert_eq!(first["source"], "invoice.txt");
    assert_eq!(first["file"], pdf.display().to_string());
    assert!(first["pages"]["first"].as_u64().unwrap() >= 1);
    assert!(first["text"].as_str().unwrap().contains("3 March"));
}

#[test]
fn ask_reads_every_pdf_in_a_directory() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = converted(dir.path());
    fs::copy(&pdf, pdf.with_file_name("copy.pdf")).unwrap();
    let (doc, _) = ask_json(
        anytopdf()
            .arg("ask")
            .arg(pdf.parent().unwrap())
            .arg("cafeteria lunch")
            .arg("--top=5"),
    );
    let files: Vec<&str> = doc["passages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["file"].as_str().unwrap())
        .collect();
    assert_eq!(files.len(), 2, "{doc}");
    assert!(files.iter().any(|f| f.ends_with("copy.pdf")));
    assert!(files.iter().any(|f| f.ends_with("notes.pdf")));
}

#[test]
fn ask_human_output_lists_passages_when_nothing_matches_or_matches() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = converted(dir.path());
    let out = anytopdf()
        .arg("ask")
        .arg(&pdf)
        .arg("spring roadmap")
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("[1] "), "{text}");
    assert!(text.contains("(roadmap.txt)"), "{text}");
    let out = anytopdf()
        .arg("ask")
        .arg(&pdf)
        .arg("zebra giraffe")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "No passage matches the question.\n"
    );
}

/// Serve one OpenAI-compatible chat completion and hand back the request it received.
fn fake_llm(reply: Value) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut head = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse().unwrap();
            }
            head.push_str(&line);
            if line == "\r\n" || line.is_empty() {
                break;
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let payload = reply.to_string();
        let mut stream = stream;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        )
        .unwrap();
        head + &String::from_utf8(body).unwrap()
    });
    (url, handle)
}

#[test]
fn ask_with_llm_answers_and_cites_passages() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = converted(dir.path());
    let (url, server) = fake_llm(json!({
        "choices": [{"message": {"role": "assistant", "content": "It is due on 3 March [1]."}}]
    }));
    let (doc, _) = ask_json(
        anytopdf()
            .env("ANYTOPDF_LLM_URL", &url)
            .env("ANYTOPDF_LLM_MODEL", "tiny")
            .env("ANYTOPDF_LLM_API_KEY", "sekret")
            .arg("ask")
            .arg(&pdf)
            .arg("When is the Acme invoice due?"),
    );
    let request = server.join().unwrap();
    assert!(
        request.starts_with("POST /v1/chat/completions "),
        "{request}"
    );
    assert!(request.contains("Bearer sekret"), "{request}");
    assert!(request.contains("\"model\":\"tiny\""), "{request}");
    assert!(request.contains("Invoice 42 from Acme"), "{request}");
    assert_eq!(doc["mode"], "llm");
    assert_eq!(doc["model"], "tiny");
    assert_eq!(doc["answer"], "It is due on 3 March [1].");
    assert_eq!(doc["cited"], json!([1]));
    assert_eq!(doc["passages"][0]["source"], "invoice.txt");
}

#[test]
fn ask_falls_back_to_passages_when_the_llm_is_unreachable() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = converted(dir.path());
    // Bind and drop a listener so the port is very likely closed.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let (doc, out) = ask_json(
        anytopdf()
            .env("ANYTOPDF_LLM_URL", format!("http://127.0.0.1:{port}/v1"))
            .env("ANYTOPDF_LLM_TIMEOUT", "5")
            .arg("ask")
            .arg(&pdf)
            .arg("invoice"),
    );
    assert_eq!(doc["mode"], "retrieval");
    assert_eq!(doc["warnings"][0]["code"], "ask.llm-failed");
    assert!(String::from_utf8_lossy(&out.stderr).contains("[ask.llm-failed]"));
    assert_eq!(doc["passages"][0]["source"], "invoice.txt");
}

#[test]
fn ask_rejects_bad_input_with_documented_exit_codes() {
    let dir = tempfile::tempdir().unwrap();
    let missing = anytopdf()
        .arg("ask")
        .arg(dir.path().join("missing.pdf"))
        .arg("anything")
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(3));
    let empty_dir = anytopdf()
        .arg("ask")
        .arg(dir.path())
        .arg("anything")
        .output()
        .unwrap();
    assert_eq!(empty_dir.status.code(), Some(3));
    let pdf = converted(dir.path());
    let blank = anytopdf().arg("ask").arg(&pdf).arg("  ").output().unwrap();
    assert_eq!(blank.status.code(), Some(2));
    let bad_url = anytopdf()
        .env("ANYTOPDF_LLM_URL", "ftp://example")
        .arg("ask")
        .arg(&pdf)
        .arg("invoice")
        .output()
        .unwrap();
    assert_eq!(bad_url.status.code(), Some(2));
}
