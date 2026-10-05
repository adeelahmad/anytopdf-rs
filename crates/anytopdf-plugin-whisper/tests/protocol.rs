use serde_json::{Value, json};
use std::{fs, path::Path, process::Command};

fn plugin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf-plugin-whisper"));
    for name in [
        "ANYTOPDF_WHISPER_ENGINE",
        "ANYTOPDF_WHISPER_BIN",
        "ANYTOPDF_WHISPER_MODEL",
        "ANYTOPDF_WHISPER_LANGUAGE",
    ] {
        command.env_remove(name);
    }
    command
}

fn audio_graph(path: &Path) -> Value {
    json!({
        "sources": [{"id": "11111111-1111-4111-8111-111111111111",
                     "path": path, "detected_type": "audio/x-wav", "metadata": {}}],
        "units": [{"id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
                   "source_id": "11111111-1111-4111-8111-111111111111", "kind": "audio",
                   "visual_path": null, "visible_text": "placeholder", "time_range": null,
                   "annotations": [], "metadata": {}}],
        "metadata": {}
    })
}

fn exchange(command: &mut Command, workspace: &Path, graph: Value) -> Value {
    let request = workspace.join("request.json");
    let response = workspace.join("response.json");
    fs::write(
        &request,
        serde_json::to_vec(&json!({
            "protocol": 1, "operation": "graph-enrich", "workspace": workspace,
            "source": null, "unit": null, "graph": graph, "output": null,
            "future_request_field": true
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

#[test]
fn manifest_declares_a_protocol_v1_graph_enricher() {
    let output = plugin().arg("--anytopdf-manifest").output().unwrap();
    assert!(output.status.success());
    let manifest: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(manifest["protocol"], 1);
    assert_eq!(manifest["name"], "whisper");
    assert_eq!(manifest["capabilities"][0]["kind"], "graph-enricher");
}

#[test]
fn graphs_without_media_come_back_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let graph = json!({"sources": [], "units": [], "metadata": {}});
    let response = exchange(&mut plugin(), dir.path(), graph);
    assert_eq!(response["ok"], true);
    assert!(response.get("graph").is_none());
}

#[test]
fn missing_engine_is_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("talk.wav");
    fs::write(&audio, b"RIFF").unwrap();
    let mut command = plugin();
    command.env("PATH", "");
    let response = exchange(&mut command, dir.path(), audio_graph(&audio));
    assert_eq!(response["ok"], true);
    assert!(response.get("graph").is_none());
    let warning = response["warnings"][0].as_str().unwrap();
    assert!(warning.contains("no Whisper engine"), "{warning}");
}

#[test]
fn unsupported_operations_return_an_error_response() {
    let dir = tempfile::tempdir().unwrap();
    let request = dir.path().join("request.json");
    let response = dir.path().join("response.json");
    fs::write(
        &request,
        r#"{"protocol":1,"operation":"import","workspace":"/tmp"}"#,
    )
    .unwrap();
    let status = plugin()
        .arg("--anytopdf-request")
        .arg(&request)
        .arg("--anytopdf-response")
        .arg(&response)
        .status()
        .unwrap();
    assert!(status.success());
    let body: Value = serde_json::from_slice(&fs::read(&response).unwrap()).unwrap();
    assert_eq!(body["ok"], false);
    assert!(body["error"].as_str().unwrap().contains("operation"));
}

#[cfg(unix)]
#[test]
fn openai_compatible_engine_output_becomes_a_transcript_unit() {
    use std::os::unix::fs::PermissionsExt;
    let Ok(ffmpeg) = which::which("ffmpeg") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("talk.wav");
    let generated = Command::new(ffmpeg)
        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("sine=frequency=440:duration=1")
        .arg(&audio)
        .status()
        .unwrap();
    assert!(generated.success());
    // Stands in for `whisper`: records its arguments and writes OpenAI-style JSON.
    let engine = dir.path().join("whisper");
    fs::write(
        &engine,
        "#!/bin/sh\n\
         echo \"$@\" > \"$(dirname \"$0\")/engine-args\"\n\
         while [ $# -gt 0 ]; do\n\
           if [ \"$1\" = --output_dir ]; then out=\"$2\"; fi\n\
           shift\n\
         done\n\
         printf '{\"language\":\"en\",\"segments\":[{\"start\":0.0,\"end\":0.8,\"text\":\" Quarterly invoice review\"}]}' > \"$out/audio.json\"\n",
    )
    .unwrap();
    fs::set_permissions(&engine, fs::Permissions::from_mode(0o755)).unwrap();

    let mut command = plugin();
    command
        .env("ANYTOPDF_WHISPER_BIN", &engine)
        .env("ANYTOPDF_WHISPER_MODEL", "tiny")
        .env("ANYTOPDF_WHISPER_LANGUAGE", "en");
    let response = exchange(&mut command, dir.path(), audio_graph(&audio));
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["warnings"], json!([]));
    let units = response["graph"]["units"].as_array().unwrap();
    assert_eq!(units.len(), 2);
    assert_eq!(
        units[0]["visible_text"],
        "Audio source: talk.wav\n\nTranscript follows."
    );
    assert_eq!(
        units[1]["visible_text"],
        "[00:00:00.000 --> 00:00:00.800] Quarterly invoice review"
    );
    assert_eq!(units[1]["annotations"][0]["provider"], "whisper:tiny");
    let args = fs::read_to_string(dir.path().join("engine-args")).unwrap();
    assert!(args.contains("--model tiny"), "{args}");
    assert!(args.contains("--language en"), "{args}");
    let workspace_audio = dir
        .path()
        .join("whisper-11111111-1111-4111-8111-111111111111/audio.wav");
    assert!(workspace_audio.is_file());
}
