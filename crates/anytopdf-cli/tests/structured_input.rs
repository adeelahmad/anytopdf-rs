use std::{fs, path::Path, process::Command};

use anytopdf_pdf::read_embedded_files;
use serde_json::Value;

#[path = "common/schema_assert.rs"]
mod schema_assert;
#[path = "common/schema.rs"]
mod schema_files;
use schema_assert::assert_valid;

const RECORDS: usize = 300;

fn convert(dir: &Path, input: &str, renderer: &str) -> Value {
    let pdf = dir.join(format!("{renderer}.pdf"));
    let output = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .current_dir(dir)
        .arg("--no-plugins")
        .env("PATH", "")
        .arg("convert")
        .arg(input)
        .args(["--renderer", renderer, "--ocr", "off", "-o"])
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let files = read_embedded_files(&fs::read(&pdf).unwrap()).unwrap();
    let chunks = files
        .iter()
        .find(|f| f.name == "anytopdf-chunks.json")
        .expect("chunks attachment");
    serde_json::from_slice(&chunks.bytes).unwrap()
}

#[test]
fn json_lines_records_become_chunks_that_share_pages() {
    let dir = tempfile::tempdir().unwrap();
    let lines: String = (0..RECORDS)
        .map(|i| format!("{{\"id\":{i},\"user\":{{\"name\":\"user-{i:03}\"}}}}\n"))
        .collect();
    fs::write(dir.path().join("events.jsonl"), &lines).unwrap();
    for renderer in ["pdfa", "pdf"] {
        let chunks = convert(dir.path(), "events.jsonl", renderer);
        assert_valid("chunks", &chunks);
        let chunks = chunks["chunks"].as_array().unwrap();
        assert_eq!(chunks.len(), RECORDS, "{renderer}");
        let last_page = chunks
            .iter()
            .map(|c| c["pages"]["last"].as_u64().unwrap())
            .max()
            .unwrap();
        // Four rows per record (heading, two fields, separator), 59 rows a page.
        assert!(last_page <= 25, "{renderer}: {last_page} pages");
        let seventh = &chunks[7];
        assert_eq!(
            seventh["text"], "Record 8 (line 8)\nid: 7\nuser.name: user-007",
            "{renderer}"
        );
        let start = seventh["anchor"]["start"].as_u64().unwrap() as usize;
        let end = seventh["anchor"]["end"].as_u64().unwrap() as usize;
        assert_eq!(
            &lines[start..end],
            r#"{"id":7,"user":{"name":"user-007"}}"#,
            "{renderer}"
        );
    }
}

#[test]
fn api_response_splits_into_envelope_and_record_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let rows: Vec<String> = (0..RECORDS)
        .map(|i| format!(r#"{{"sku":"S-{i}","qty":{i}}}"#))
        .collect();
    fs::write(
        dir.path().join("api.json"),
        format!(r#"{{"next":"cursor-1","data":[{}]}}"#, rows.join(",")),
    )
    .unwrap();
    let chunks = convert(dir.path(), "api.json", "pdfa");
    let texts: Vec<&str> = chunks["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts.len(), RECORDS + 1);
    assert_eq!(
        texts[..3],
        [
            "next: cursor-1\ndata: [300 records below]",
            "Record 1 (data[0])\nsku: S-0\nqty: 0",
            "Record 2 (data[1])\nsku: S-1\nqty: 1",
        ]
    );
}
