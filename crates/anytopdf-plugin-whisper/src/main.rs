//! `anytopdf-plugin-whisper`: an anytopdf runtime plugin (protocol v1) that
//! transcribes audio and video sources with Whisper and adds a timed,
//! searchable transcript unit for each one.
//!
//! It registers as a graph enricher, extracts each source's audio with FFmpeg
//! into the job workspace, and runs whisper.cpp or an OpenAI-compatible
//! Whisper CLI. Sources that already carry a transcript are left alone.

mod backend;
mod transcript;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const PROTOCOL: u64 = 1;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--anytopdf-manifest" => {
            println!("{}", manifest());
            ExitCode::SUCCESS
        }
        [flag] if flag == "--version" => {
            println!("anytopdf-plugin-whisper {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [req_flag, request, resp_flag, response]
            if req_flag == "--anytopdf-request" && resp_flag == "--anytopdf-response" =>
        {
            let body = handle(Path::new(request)).unwrap_or_else(|e| {
                json!({"protocol": PROTOCOL, "ok": false, "warnings": [], "error": format!("{e:#}")})
            });
            match write_atomic(Path::new(response), &body) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("anytopdf-plugin-whisper: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: anytopdf-plugin-whisper --anytopdf-manifest\n       \
                 anytopdf-plugin-whisper --anytopdf-request REQUEST --anytopdf-response RESPONSE"
            );
            ExitCode::from(2)
        }
    }
}

fn manifest() -> Value {
    json!({
        "protocol": PROTOCOL,
        "name": "whisper",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": [{
            "kind": "graph-enricher",
            "extensions": [],
            "mime_types": ["audio/*", "video/*"],
            "priority": 50
        }]
    })
}

fn handle(request_path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(request_path).context("read request")?;
    let request: Value = serde_json::from_str(&raw).context("parse request")?;
    if request["protocol"].as_u64() != Some(PROTOCOL) {
        bail!("unsupported protocol {}", request["protocol"]);
    }
    if request["operation"] != "graph-enrich" {
        bail!("unsupported operation {}", request["operation"]);
    }
    let workspace = PathBuf::from(
        request["workspace"]
            .as_str()
            .context("request has no workspace")?,
    );
    let mut graph = request["graph"].clone();
    if !graph.is_object() {
        bail!("graph-enrich request has no graph");
    }

    let sources = transcript::media_sources(&graph);
    let mut response = json!({"protocol": PROTOCOL, "ok": true, "warnings": []});
    if sources.is_empty() {
        return Ok(response);
    }
    let mut warnings = Vec::new();
    let backend =
        match backend::detect(&backend::Config::from_env(), |name| which::which(name).ok()) {
            Ok(backend) => backend,
            Err(e) => {
                warnings.push(format!(
                    "whisper: {e:#}; {} media source(s) not transcribed",
                    sources.len()
                ));
                response["warnings"] = json!(warnings);
                return Ok(response);
            }
        };
    let Ok(ffmpeg) = which::which("ffmpeg") else {
        warnings.push("whisper: ffmpeg is required to read audio; nothing transcribed".into());
        response["warnings"] = json!(warnings);
        return Ok(response);
    };

    let provider = backend.provider();
    let mut changed = false;
    for source in &sources {
        let dir = workspace.join(format!("whisper-{}", source.id));
        let result = fs::create_dir_all(&dir)
            .context("create work directory")
            .and_then(|()| extract_audio(&ffmpeg, &source.path, &dir))
            .and_then(|wav| backend.transcribe(&wav, &dir));
        match result {
            Ok((segments, language)) => {
                if transcript::apply(
                    &mut graph,
                    source,
                    &segments,
                    &provider,
                    language.as_deref(),
                ) {
                    changed = true;
                } else {
                    warnings.push(format!("whisper: no speech recognized in {}", source.name));
                }
            }
            Err(e) => warnings.push(format!("whisper: {}: {e:#}", source.name)),
        }
    }
    response["warnings"] = json!(warnings);
    if changed {
        response["graph"] = graph;
    }
    Ok(response)
}

/// Decodes the first audio stream to the 16 kHz mono PCM WAV Whisper expects.
fn extract_audio(ffmpeg: &Path, input: &Path, dir: &Path) -> Result<PathBuf> {
    let wav = dir.join("audio.wav");
    let output = Command::new(ffmpeg)
        .args(["-nostdin", "-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(input)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-c:a",
            "pcm_s16le",
        ])
        .arg(&wav)
        .output()
        .context("run ffmpeg")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("matches no streams") {
            bail!("no audio track");
        }
        bail!(
            "ffmpeg could not decode audio: {}",
            stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
        );
    }
    Ok(wav)
}

fn write_atomic(path: &Path, body: &Value) -> Result<()> {
    let mut name = path
        .file_name()
        .context("response path has no file name")?
        .to_os_string();
    name.push(".tmp");
    let temporary = path.with_file_name(name);
    fs::write(&temporary, serde_json::to_vec(body)?).context("write response")?;
    fs::rename(&temporary, path).context("publish response")?;
    Ok(())
}
