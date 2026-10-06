use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

#[path = "common/process.rs"]
mod process;

/// Send newline-delimited JSON-RPC messages to `anytopdf mcp` and collect every reply.
fn session(messages: &[Value], cwd: &Path) -> Vec<Value> {
    let mut command: Command = process::command();
    let mut child = command
        .arg("mcp")
        .current_dir(cwd)
        .env_remove("SOURCE_DATE_EPOCH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    for message in messages {
        writeln!(stdin, "{message}").unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "mcp server failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).expect("every stdout line is JSON-RPC"))
        .collect()
}

fn initialize() -> Value {
    json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "test", "version": "0"}
    }})
}

fn call(id: u64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": name, "arguments": arguments}})
}

fn reply(replies: &[Value], id: u64) -> &Value {
    replies
        .iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no reply for id {id}: {replies:?}"))
}

#[test]
fn mcp_handshake_lists_the_five_tools_and_ignores_notifications() {
    let dir = tempfile::tempdir().unwrap();
    let replies = session(
        &[
            initialize(),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
        ],
        dir.path(),
    );
    assert_eq!(replies.len(), 3, "notifications must not be answered");
    let init = &reply(&replies, 0)["result"];
    assert_eq!(init["protocolVersion"], "2025-06-18");
    assert_eq!(init["serverInfo"]["name"], "anytopdf");
    assert!(init["capabilities"]["tools"].is_object());
    let names: Vec<&str> = reply(&replies, 1)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["convert", "extract", "probe", "search", "capabilities"]
    );
    assert_eq!(reply(&replies, 2)["result"], json!({}));
}

#[test]
fn mcp_convert_then_extract_round_trips_through_the_cli() {
    let dir = tempfile::tempdir().unwrap();
    let notes = dir.path().join("notes.txt");
    fs::write(&notes, "MCP fixture line\n").unwrap();
    let pdf = dir.path().join("out.pdf");
    let replies = session(
        &[
            initialize(),
            call(1, "probe", json!({"input": notes})),
            call(
                2,
                "convert",
                json!({"inputs": [notes], "output": pdf, "ocr": "off"}),
            ),
            call(3, "extract", json!({"pdf": pdf})),
            call(4, "capabilities", json!({})),
        ],
        dir.path(),
    );
    for id in 1..=4 {
        let result = &reply(&replies, id)["result"];
        assert_eq!(result["isError"], false, "call {id} failed: {result}");
        let text = result["content"][0]["text"].as_str().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(text).unwrap(),
            result["structuredContent"]
        );
    }
    assert_eq!(
        reply(&replies, 1)["result"]["structuredContent"]["schema_version"],
        "anytopdf.probe/1"
    );
    assert!(pdf.is_file(), "convert must publish the PDF");
    let extract = &reply(&replies, 3)["result"]["structuredContent"];
    assert_eq!(extract["origin"], "embedded");
    assert!(extract["chunks"].is_object());
    assert_eq!(
        reply(&replies, 4)["result"]["structuredContent"]["schema_version"],
        "anytopdf.capabilities/1"
    );
}

#[test]
fn mcp_reports_cli_failures_as_tool_errors_with_the_exit_class() {
    let dir = tempfile::tempdir().unwrap();
    let existing = dir.path().join("keep.pdf");
    fs::write(&existing, "keep").unwrap();
    let notes = dir.path().join("notes.txt");
    fs::write(&notes, "text\n").unwrap();
    let replies = session(
        &[
            initialize(),
            call(1, "extract", json!({"pdf": dir.path().join("missing.pdf")})),
            call(
                2,
                "convert",
                json!({"inputs": [notes], "output": existing, "ocr": "off"}),
            ),
        ],
        dir.path(),
    );
    let missing = &reply(&replies, 1)["result"];
    assert_eq!(missing["isError"], true);
    assert!(
        missing["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("anytopdf exited with 3 (input)"),
        "{missing}"
    );
    assert_eq!(reply(&replies, 2)["result"]["isError"], true);
    assert_eq!(fs::read_to_string(&existing).unwrap(), "keep");
}

#[test]
fn mcp_protocol_errors_use_json_rpc_codes() {
    let dir = tempfile::tempdir().unwrap();
    let replies = session(
        &[
            json!("not an object"),
            json!({"jsonrpc": "2.0", "id": 1, "method": "resources/list"}),
            call(2, "shell", json!({})),
            call(
                3,
                "convert",
                json!({"inputs": ["a"], "dump_graph": "g.json"}),
            ),
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "probe", "arguments": []}}),
        ],
        dir.path(),
    );
    assert_eq!(replies[0]["error"]["code"], -32600);
    assert_eq!(reply(&replies, 1)["error"]["code"], -32601);
    for id in 2..=4 {
        assert_eq!(reply(&replies, id)["error"]["code"], -32602, "id {id}");
    }
}

#[test]
fn mcp_answers_malformed_json_and_invalid_utf8_then_keeps_serving() {
    let dir = tempfile::tempdir().unwrap();
    let mut command = process::command();
    let mut child = command
        .arg("mcp")
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"{not json\n\"\xff\"\n{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 3, "{replies:?}");
    for parse_error in &replies[..2] {
        assert_eq!(parse_error["error"]["code"], -32700);
        assert_eq!(parse_error["id"], Value::Null);
    }
    assert_eq!(replies[2]["id"], 7);
    assert_eq!(replies[2]["result"], json!({}));
}
