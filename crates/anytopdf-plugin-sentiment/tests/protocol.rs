use anytopdf_core::{Annotation, DocumentGraph, SourceRecord, Unit};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpListener,
    path::Path,
    process::Command,
    sync::mpsc,
    thread,
};

fn plugin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf-plugin-sentiment"));
    for (name, _) in std::env::vars() {
        if name.starts_with("ANYTOPDF_SENTIMENT_") || name.starts_with("ANYTOPDF_LLM_") {
            command.env_remove(name);
        }
    }
    command
}

fn transcript_unit() -> Value {
    let cue = |text: &str, start: f64| {
        json!({"kind": "transcript", "text": text, "provider": "whisper.cpp", "confidence": null,
               "region": null, "time_range": {"start_seconds": start, "end_seconds": start + 3.0},
               "attributes": {"speaker": "SPEAKER_00"}})
    };
    json!({
        "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "source_id": "11111111-1111-4111-8111-111111111111",
        "kind": "text", "visual_path": null, "time_range": null,
        "visible_text": "[00:00:00.000 --> 00:00:03.000] ...",
        "annotations": [
            cue("Welcome everyone, this is a wonderful milestone.", 0.0),
            cue("The checkout page is broken and customers keep asking for a refund.", 3.0),
            cue("Can we fix it today?", 6.0)
        ],
        "metadata": {}, "future_unit_field": 1
    })
}

fn exchange(command: &mut Command, workspace: &Path, unit: Value) -> Value {
    let request = workspace.join("request.json");
    let response = workspace.join("response.json");
    fs::write(
        &request,
        serde_json::to_vec(&json!({
            "protocol": 1, "operation": "unit-enrich", "workspace": workspace,
            "source": {"id": "11111111-1111-4111-8111-111111111111", "path": "/media/call.mp3",
                       "detected_type": "audio/mpeg", "metadata": {}},
            "unit": unit, "graph": null, "output": null, "future_request_field": true
        }))
        .unwrap(),
    )
    .unwrap();
    let status = command
        .arg("--anytopdf-request")
        .arg(&request)
        .arg("--anytopdf-response")
        .arg(&response)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!workspace.join("response.json.tmp").exists());
    serde_json::from_slice(&fs::read(&response).unwrap()).unwrap()
}

/// Appends the response's annotations to the unit and validates the result
/// the way the host does.
fn host_accepts(unit: &Value, response: &Value) -> Vec<Annotation> {
    let mut unit: Unit = serde_json::from_value(unit.clone()).unwrap();
    let added: Vec<Annotation> = serde_json::from_value(response["annotations"].clone()).unwrap();
    unit.annotations.extend(added.clone());
    let mut source = SourceRecord::new("/media/call.mp3".into());
    source.id = unit.source_id;
    DocumentGraph {
        sources: vec![source],
        units: vec![unit],
        ..Default::default()
    }
    .validate()
    .unwrap();
    added
}

fn summary(annotations: &[Annotation]) -> Vec<(String, String)> {
    annotations
        .iter()
        .map(|a| (a.attributes["entity"].clone(), a.text.clone()))
        .collect()
}

#[test]
fn manifest_declares_a_protocol_v1_unit_enricher_for_every_unit() {
    let output = plugin().arg("--anytopdf-manifest").output().unwrap();
    assert!(output.status.success());
    let manifest: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(manifest["protocol"], 1);
    assert_eq!(manifest["name"], "sentiment");
    let capability = &manifest["capabilities"][0];
    assert_eq!(capability["kind"], "unit-enricher");
    assert_eq!(capability["extensions"], json!([]));
    assert_eq!(capability["mime_types"], json!([]));
}

#[test]
fn transcript_segments_are_labelled_with_the_lexicon_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let unit = transcript_unit();
    let response = exchange(&mut plugin(), dir.path(), unit.clone());
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["warnings"], json!([]));
    assert!(
        response.get("unit").is_none(),
        "annotations are append-only"
    );
    let added = host_accepts(&unit, &response);
    let pairs = summary(&added);
    let pairs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("sentiment", "positive"),
            ("sentiment", "negative"),
            ("tone", "complaint"),
            ("tone", "question"),
            ("sentiment-overall", "overall neutral"),
        ]
    );
    assert!(added.iter().all(|a| a.provider == "sentiment-vader"));
    // Labels describe text segments only: speaker labels are not copied.
    assert!(added.iter().all(|a| !a.attributes.contains_key("speaker")));
    let complaint = &added[2];
    let time = complaint.time_range.unwrap();
    assert_eq!((time.start_seconds, time.end_seconds), (3.0, 6.0));
}

#[test]
fn units_without_text_get_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let unit = json!({
        "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "source_id": "11111111-1111-4111-8111-111111111111",
        "kind": "visual", "visual_path": "/media/photo.jpg", "visible_text": null,
        "time_range": null, "annotations": [], "metadata": {}
    });
    let response = exchange(&mut plugin(), dir.path(), unit);
    assert_eq!(response["ok"], true);
    assert_eq!(response["annotations"], json!([]));
}

#[test]
fn invalid_settings_are_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = plugin();
    command.env("ANYTOPDF_SENTIMENT_BACKEND", "bert");
    let response = exchange(&mut command, dir.path(), transcript_unit());
    assert_eq!(response["ok"], true);
    assert_eq!(response["annotations"], json!([]));
    let warning = response["warnings"][0].as_str().unwrap();
    assert!(warning.contains("ANYTOPDF_SENTIMENT_BACKEND"), "{warning}");
}

/// Serves one OpenAI-compatible chat completion and hands back the request
/// line, headers and body it received.
fn fake_llm(content: &str) -> (String, mpsc::Receiver<(String, String)>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let reply =
        json!({"choices": [{"message": {"role": "assistant", "content": content}}]}).to_string();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        let mut length = 0;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = v.trim().parse().unwrap();
            }
            if line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        let mut stream = reader.into_inner();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        )
        .unwrap();
        tx.send((head, String::from_utf8(body).unwrap())).unwrap();
    });
    (url, rx)
}

#[test]
fn an_llm_endpoint_labels_segments_and_adds_formal_and_informal_tones() {
    let (url, received) = fake_llm(
        r#"{"results": [
            {"i": 0, "label": "positive", "confidence": 0.93, "tones": ["formal"]},
            {"i": 1, "label": "negative", "confidence": 0.88, "tones": ["complaint", "urgent"]},
            {"i": 2, "label": "neutral", "confidence": 0.7, "tones": ["question", "informal"]}
        ]}"#,
    );
    let dir = tempfile::tempdir().unwrap();
    let mut command = plugin();
    command
        .env("ANYTOPDF_SENTIMENT_LLM_URL", &url)
        .env("ANYTOPDF_SENTIMENT_LLM_MODEL", "qwen2.5:3b")
        .env("ANYTOPDF_SENTIMENT_LLM_API_KEY", "local-key");
    let unit = transcript_unit();
    let response = exchange(&mut command, dir.path(), unit.clone());
    assert_eq!(response["warnings"], json!([]), "{response}");
    let added = host_accepts(&unit, &response);
    let pairs = summary(&added);
    let pairs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("sentiment", "positive"),
            ("tone", "formal"),
            ("sentiment", "negative"),
            ("tone", "complaint"),
            ("tone", "urgent"),
            ("tone", "question"),
            ("tone", "informal"),
            ("sentiment-overall", "overall negative"),
        ]
    );
    assert!(added.iter().all(|a| a.provider == "sentiment-llm"));
    assert!(added.iter().all(|a| a.attributes["model"] == "qwen2.5:3b"));
    assert_eq!(added[2].confidence, Some(0.88));

    let (head, body) = received.recv().unwrap();
    assert!(head.starts_with("POST /v1/chat/completions "), "{head}");
    assert!(head.contains("Bearer local-key"), "{head}");
    let body: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(body["model"], "qwen2.5:3b");
    let prompt: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    assert_eq!(prompt["segments"][2]["text"], "Can we fix it today?");
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Never guess"), "{system}");
}

#[test]
fn request_options_override_environment_settings() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = plugin();
    command.env("ANYTOPDF_SENTIMENT_BACKEND", "bert");
    let request = dir.path().join("request.json");
    let response = dir.path().join("response.json");
    fs::write(
        &request,
        serde_json::to_vec(&json!({
            "protocol": 1, "operation": "unit-enrich", "workspace": dir.path(),
            "unit": transcript_unit(),
            "options": {"backend": "vader", "neutral": true, "tones": false,
                        "from": ["transcript"], "threshold": 0.05}
        }))
        .unwrap(),
    )
    .unwrap();
    let status = command
        .arg("--anytopdf-request")
        .arg(&request)
        .arg("--anytopdf-response")
        .arg(&response)
        .status()
        .unwrap();
    assert!(status.success());
    let response: Value = serde_json::from_slice(&fs::read(&response).unwrap()).unwrap();
    assert_eq!(response["warnings"], json!([]), "{response}");
    let added = host_accepts(&transcript_unit(), &response);
    let pairs = summary(&added);
    let pairs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    assert_eq!(
        pairs,
        [
            ("sentiment", "positive"),
            ("sentiment", "negative"),
            ("sentiment", "neutral"),
            ("sentiment-overall", "overall neutral"),
        ]
    );
}
