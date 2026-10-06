//! Model Context Protocol server over stdio.
//!
//! `anytopdf mcp` reads newline-delimited JSON-RPC 2.0 messages on stdin and
//! answers on stdout. Each tool call re-runs this executable with the matching
//! subcommand and `--json`, so tools keep the CLI's exact behaviour, exit codes and
//! schemas, and a failing conversion can never write stray bytes into the protocol
//! stream.

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::exit::ExitClass;

/// Protocol revisions this server speaks, newest first.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

/// Global flags the server forwards to every tool invocation.
pub(crate) struct Forwarded(pub(crate) Vec<String>);

impl Forwarded {
    pub(crate) fn from_cli(cli: &crate::cli::Cli) -> Self {
        let mut flags = Vec::new();
        if cli.no_plugins {
            flags.push("--no-plugins".into());
        }
        flags.push(format!("--plugin-timeout={}", cli.plugin_timeout));
        for kind in &cli.allow_plugin_kind {
            flags.push(format!("--allow-plugin-kind={kind}"));
        }
        for kind in &cli.deny_plugin_kind {
            flags.push(format!("--deny-plugin-kind={kind}"));
        }
        flags.push(format!("--plugin-sandbox={}", cli.sandbox_mode()));
        for path in &cli.plugin_sandbox_allow_read {
            flags.push(format!("--plugin-sandbox-allow-read={}", path.display()));
        }
        flags.extend(
            crate::config::forward_flags(cli)
                .into_iter()
                .map(|flag| flag.to_string_lossy().into_owned()),
        );
        Forwarded(flags)
    }
}

struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
        }
    }
}

pub(crate) fn serve(forwarded: Forwarded) -> Result<()> {
    let exe = std::env::current_exe().context("cannot locate the anytopdf executable")?;
    let server = Server { exe, forwarded };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut input = stdin.lock();
    let mut line = Vec::new();
    loop {
        line.clear();
        if input
            .read_until(b'\n', &mut line)
            .context("reading MCP request")?
            == 0
        {
            return Ok(());
        }
        if line.trim_ascii().is_empty() {
            continue;
        }
        if let Some(response) = server.handle_line(&line) {
            let mut text = serde_json::to_string(&response)?;
            text.push('\n');
            if stdout
                .write_all(text.as_bytes())
                .and_then(|()| stdout.flush())
                .is_err()
            {
                // The client went away; there is nobody left to answer.
                return Ok(());
            }
        }
    }
}

struct Server {
    exe: PathBuf,
    forwarded: Forwarded,
}

impl Server {
    fn handle_line(&self, line: &[u8]) -> Option<Value> {
        // Invalid UTF-8 is a parse error for this message, not a reason to stop serving.
        let message: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(e) => return Some(error_response(Value::Null, PARSE_ERROR, e.to_string())),
        };
        if let Value::Array(batch) = message {
            // Batches were removed in 2025-06-18 but older clients may still send them.
            let replies: Vec<Value> = batch.iter().filter_map(|m| self.handle(m)).collect();
            return (!replies.is_empty()).then_some(Value::Array(replies));
        }
        self.handle(&message)
    }

    fn handle(&self, message: &Value) -> Option<Value> {
        let id = message.get("id").cloned();
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            // A response from the client, or garbage: never answer a response.
            if message.get("result").is_some() || message.get("error").is_some() {
                return None;
            }
            return Some(error_response(
                id.unwrap_or(Value::Null),
                INVALID_REQUEST,
                "missing method",
            ));
        };
        let params = message.get("params").cloned().unwrap_or(json!({}));
        let result = self.dispatch(method, &params);
        // Notifications (no id) never receive a reply, even on failure.
        let id = id?;
        Some(match result {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => error_response(id, e.code, e.message),
        })
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(initialize(params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tool_definitions()})),
            "tools/call" => self.call(params),
            m if m.starts_with("notifications/") => Ok(Value::Null),
            other => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("method not found: {other}"),
            )),
        }
    }

    fn call(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "tools/call requires a tool name"))?;
        let empty = Map::new();
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => &empty,
            Some(Value::Object(map)) => map,
            Some(_) => return Err(RpcError::new(INVALID_PARAMS, "arguments must be an object")),
        };
        let args = tool_argv(name, arguments).map_err(|e| RpcError::new(INVALID_PARAMS, e))?;
        Ok(self.run(&args))
    }

    fn run(&self, args: &[String]) -> Value {
        let output = Command::new(&self.exe)
            .args(&self.forwarded.0)
            .args(args)
            .stdin(Stdio::null())
            .output();
        let output = match output {
            Ok(output) => output,
            Err(e) => return tool_error(format!("failed to start anytopdf: {e}"), None),
        };
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let code = output.status.code();
        let structured = serde_json::from_str::<Value>(&stdout)
            .ok()
            .filter(Value::is_object);
        if code == Some(ExitClass::Success.code().into()) {
            let mut content = vec![json!({"type": "text", "text": stdout})];
            if !stderr.is_empty() {
                content.push(json!({"type": "text", "text": stderr}));
            }
            let mut result = json!({"content": content, "isError": false});
            if let Some(structured) = structured {
                result["structuredContent"] = structured;
            }
            return result;
        }
        let class = code
            .and_then(|c| {
                ExitClass::ALL
                    .into_iter()
                    .find(|e| i32::from(e.code()) == c)
            })
            .map_or("signal", ExitClass::name);
        let mut text = format!(
            "anytopdf exited with {} ({class})",
            code.map_or_else(|| "no code".into(), |c| c.to_string())
        );
        for part in [&stderr, &stdout] {
            if !part.is_empty() {
                text.push_str("\n\n");
                text.push_str(part);
            }
        }
        tool_error(text, structured)
    }
}

fn error_response(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message.into()}})
}

fn tool_error(text: String, structured: Option<Value>) -> Value {
    let mut result = json!({"content": [{"type": "text", "text": text}], "isError": true});
    if let Some(structured) = structured {
        result["structuredContent"] = structured;
    }
    result
}

fn initialize(params: &Value) -> Value {
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = requested
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "anytopdf", "version": env!("CARGO_PKG_VERSION")},
        "instructions": "Convert local media and documents into searchable PDFs and read \
            back their embedded manifest and chunks. Paths are local to the machine running \
            the server; relative paths resolve against the server's working directory, so \
            prefer absolute paths. Outputs are never overwritten unless overwrite is true, \
            and source files are always protected."
    })
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "convert",
            "title": "Convert to searchable PDF",
            "description": "Convert files or directories into one searchable PDF (or one PDF per \
                input with output_dir). Returns the anytopdf.convert/1 JSON report: outputs, \
                converted and skipped inputs, and diagnostics.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "inputs": {"type": "array", "items": {"type": "string"}, "minItems": 1,
                        "description": "Files or directories to convert."},
                    "output": {"type": "string", "description": "Output PDF path. Defaults to a name derived from the first input."},
                    "output_dir": {"type": "string", "description": "Write one PDF per input into this directory instead of one merged PDF."},
                    "overwrite": {"type": "boolean", "description": "Replace an existing output; source files are always protected."},
                    "strict": {"type": "boolean", "description": "Fail without publishing when ingestion or rendering reports warnings."},
                    "fail_fast": {"type": "boolean", "description": "Abort without publishing when any input fails."},
                    "filter": {"type": "string", "description": "Only convert discovered inputs whose path matches this regular expression."},
                    "include_hidden": {"type": "boolean", "description": "Include hidden files and directories during discovery."},
                    "ocr": {"type": "string", "enum": ["auto", "vision", "doctr", "tesseract", "off"], "description": "OCR provider selection."},
                    "lang": {"type": "string", "description": "OCR language code, for example eng."},
                    "transcripts": {"type": "array", "items": {"type": "string"}, "description": "Transcript files to attach to media."},
                    "profile": {"type": "string", "enum": ["archive", "share"], "description": "archive keeps provenance detail, share strips local paths."},
                    "no_provenance_page": {"type": "boolean", "description": "Omit the provenance page."},
                    "video_interval": {"type": "number", "exclusiveMinimum": 0, "description": "Seconds between sampled video frames."},
                    "max_video_frames": {"type": "integer", "minimum": 0, "description": "Maximum video frames to keep (0 means unlimited)."},
                    "max_image_frames": {"type": "integer", "minimum": 0, "description": "Maximum frames from a multi-frame TIFF or GIF (0 means unlimited)."}
                },
                "required": ["inputs"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false}
        },
        {
            "name": "extract",
            "title": "Extract manifest and chunks",
            "description": "Read the embedded (or sidecar) manifest and chunks from a PDF produced \
                by anytopdf. Returns the anytopdf.extract/1 document.",
            "inputSchema": {
                "type": "object",
                "properties": {"pdf": {"type": "string", "description": "PDF file to read."}},
                "required": ["pdf"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        },
        {
            "name": "probe",
            "title": "Probe an input",
            "description": "Detect the format of a file and the importer that would handle it, \
                without converting. Returns the anytopdf.probe/1 document.",
            "inputSchema": {
                "type": "object",
                "properties": {"input": {"type": "string", "description": "File to inspect."}},
                "required": ["input"],
                "additionalProperties": false
            },
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        },
        {
            "name": "capabilities",
            "title": "Describe capabilities",
            "description": "List exit codes, diagnostic codes, profiles, OCR modes, importers and \
                JSON schema ids. Returns the anytopdf.capabilities/1 document.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
            "annotations": {"readOnlyHint": true, "openWorldHint": false}
        }
    ])
}

/// Translate tool arguments into a subcommand argv. Option values always use the
/// `--flag=value` form and positional inputs follow `--`, so a value starting with
/// `-` can never be read as a flag.
fn tool_argv(name: &str, arguments: &Map<String, Value>) -> Result<Vec<String>, String> {
    let allowed: &[&str] = match name {
        "convert" => &[
            "inputs",
            "output",
            "output_dir",
            "overwrite",
            "strict",
            "fail_fast",
            "filter",
            "include_hidden",
            "ocr",
            "lang",
            "transcripts",
            "profile",
            "no_provenance_page",
            "video_interval",
            "max_video_frames",
            "max_image_frames",
        ],
        "extract" => &["pdf"],
        "probe" => &["input"],
        "capabilities" => &[],
        other => return Err(format!("unknown tool: {other}")),
    };
    if let Some(unknown) = arguments.keys().find(|k| !allowed.contains(&k.as_str())) {
        return Err(format!("{name} does not accept argument `{unknown}`"));
    }
    let mut argv = vec![name.to_string(), "--json".to_string()];
    match name {
        "extract" => {
            argv.push("--".into());
            argv.push(string(arguments, "pdf")?.ok_or("extract requires `pdf`")?);
        }
        "probe" => {
            argv.push("--".into());
            argv.push(string(arguments, "input")?.ok_or("probe requires `input`")?);
        }
        "convert" => convert_argv(arguments, &mut argv)?,
        _ => {}
    }
    Ok(argv)
}

fn convert_argv(arguments: &Map<String, Value>, argv: &mut Vec<String>) -> Result<(), String> {
    for (key, flag) in [
        ("output", "--output"),
        ("output_dir", "--output-dir"),
        ("filter", "--filter"),
        ("ocr", "--ocr"),
        ("lang", "--lang"),
        ("profile", "--profile"),
    ] {
        if let Some(value) = string(arguments, key)? {
            argv.push(format!("{flag}={value}"));
        }
    }
    for (key, flag) in [
        ("overwrite", "--overwrite"),
        ("strict", "--strict"),
        ("fail_fast", "--fail-fast"),
        ("include_hidden", "--include-hidden"),
        ("no_provenance_page", "--no-provenance-page"),
    ] {
        match arguments.get(key) {
            None | Some(Value::Null) | Some(Value::Bool(false)) => {}
            Some(Value::Bool(true)) => argv.push(flag.into()),
            Some(_) => return Err(format!("`{key}` must be a boolean")),
        }
    }
    for (key, flag) in [
        ("max_video_frames", "--max-video-frames"),
        ("max_image_frames", "--max-image-frames"),
    ] {
        match arguments.get(key) {
            None | Some(Value::Null) => {}
            Some(value) => {
                let n = value
                    .as_u64()
                    .ok_or_else(|| format!("`{key}` must be a non-negative integer"))?;
                argv.push(format!("{flag}={n}"));
            }
        }
    }
    if let Some(value) = arguments.get("video_interval").filter(|v| !v.is_null()) {
        let seconds = value
            .as_f64()
            .filter(|s| *s > 0.0 && s.is_finite())
            .ok_or("`video_interval` must be a positive number")?;
        argv.push(format!("--video-interval={seconds}"));
    }
    for transcript in strings(arguments, "transcripts")? {
        argv.push(format!("--transcript={transcript}"));
    }
    let inputs = strings(arguments, "inputs")?;
    if inputs.is_empty() {
        return Err("convert requires at least one entry in `inputs`".into());
    }
    argv.push("--".into());
    argv.extend(inputs);
    Ok(())
}

fn string(arguments: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a non-empty string")),
    }
}

fn strings(arguments: &Map<String, Value>, key: &str) -> Result<Vec<String>, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(s) if !s.is_empty() => Ok(s.clone()),
                _ => Err(format!("`{key}` must contain non-empty strings")),
            })
            .collect(),
        Some(_) => Err(format!("`{key}` must be an array of strings")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn convert_arguments_keep_dash_values_out_of_flag_position() {
        let argv = tool_argv(
            "convert",
            &args(json!({
                "inputs": ["-rf", "b.txt"], "output": "-x.pdf", "overwrite": true,
                "ocr": "off", "max_image_frames": 2, "transcripts": ["t.vtt"]
            })),
        )
        .unwrap();
        assert_eq!(
            argv,
            [
                "convert",
                "--json",
                "--output=-x.pdf",
                "--ocr=off",
                "--overwrite",
                "--max-image-frames=2",
                "--transcript=t.vtt",
                "--",
                "-rf",
                "b.txt"
            ]
        );
    }

    #[test]
    fn tool_arguments_reject_unknown_and_mistyped_values() {
        for (tool, value) in [
            ("convert", json!({})),
            ("convert", json!({"inputs": []})),
            ("convert", json!({"inputs": ["a"], "strict": "yes"})),
            ("convert", json!({"inputs": ["a"], "max_video_frames": -1})),
            ("convert", json!({"inputs": ["a"], "video_interval": 0})),
            (
                "convert",
                json!({"inputs": ["a"], "dump_graph": "/tmp/g.json"}),
            ),
            ("extract", json!({})),
            ("probe", json!({"input": 3})),
            ("capabilities", json!({"x": 1})),
            ("shell", json!({})),
        ] {
            assert!(
                tool_argv(tool, &args(value.clone())).is_err(),
                "{tool} accepted {value}"
            );
        }
    }

    #[test]
    fn initialize_negotiates_known_protocol_versions() {
        assert_eq!(
            initialize(&json!({"protocolVersion": "2024-11-05"}))["protocolVersion"],
            "2024-11-05"
        );
        assert_eq!(
            initialize(&json!({"protocolVersion": "1999-01-01"}))["protocolVersion"],
            PROTOCOL_VERSIONS[0]
        );
    }

    #[test]
    fn every_tool_definition_matches_the_argument_allowlist() {
        for tool in tool_definitions().as_array().unwrap() {
            let name = tool["name"].as_str().unwrap();
            let properties = tool["inputSchema"]["properties"].as_object().unwrap();
            let unknown = json!({"__unknown__": true});
            assert!(tool_argv(name, &args(unknown)).is_err());
            for key in properties.keys() {
                let err = tool_argv(name, &args(json!({key: null})))
                    .err()
                    .unwrap_or_default();
                assert!(
                    !err.contains("does not accept"),
                    "{name} schema lists `{key}` but the allowlist rejects it"
                );
            }
        }
    }
}
