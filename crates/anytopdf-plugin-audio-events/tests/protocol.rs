use serde_json::{Value, json};
use std::{f32::consts::TAU, fs, path::Path, process::Command};

fn plugin() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf-plugin-audio-events"));
    for name in [
        "ANYTOPDF_AUDIO_EVENTS_MODEL",
        "ANYTOPDF_AUDIO_EVENTS_LABELS",
        "ANYTOPDF_AUDIO_EVENTS_DEVICE",
        "ANYTOPDF_AUDIO_EVENTS_THRESHOLD",
        "ANYTOPDF_AUDIO_EVENTS_RAISED_LU",
        "ORT_DYLIB_PATH",
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

/// Writes a 16-bit mono WAV: three seconds of a steady tone, then four of
/// digital silence.
fn write_wav(path: &Path) {
    let rate = 16_000u32;
    let mut samples: Vec<i16> = (0..rate * 3)
        .map(|i| (0.3 * (TAU * 440.0 * i as f32 / rate as f32).sin() * 32767.0) as i16)
        .collect();
    samples.extend(std::iter::repeat_n(0, rate as usize * 4));
    let data_len = samples.len() as u32 * 2;
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
    wav.extend((36 + data_len).to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(rate.to_le_bytes());
    wav.extend((rate * 2).to_le_bytes());
    wav.extend(2u16.to_le_bytes());
    wav.extend(16u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(data_len.to_le_bytes());
    wav.extend(samples.iter().flat_map(|s| s.to_le_bytes()));
    fs::write(path, wav).unwrap();
}

#[test]
fn manifest_declares_a_protocol_v1_graph_enricher() {
    let output = plugin().arg("--anytopdf-manifest").output().unwrap();
    assert!(output.status.success());
    let manifest: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(manifest["protocol"], 1);
    assert_eq!(manifest["name"], "audio-events");
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
fn missing_ffmpeg_is_a_warning_not_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("talk.wav");
    write_wav(&audio);
    let mut command = plugin();
    command.env("PATH", "");
    let response = exchange(&mut command, dir.path(), audio_graph(&audio));
    assert_eq!(response["ok"], true);
    assert!(response.get("graph").is_none());
    assert!(
        response["warnings"][0]
            .as_str()
            .unwrap()
            .contains("ffmpeg is required"),
        "{response}"
    );
}

#[test]
fn unsupported_operations_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let request = dir.path().join("request.json");
    let response = dir.path().join("response.json");
    fs::write(&request, r#"{"protocol": 1, "operation": "import"}"#).unwrap();
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

/// Needs FFmpeg on PATH; skipped (passes) without it.
#[test]
fn wav_gets_an_audio_events_unit_and_a_broken_model_only_warns() {
    if which::which("ffmpeg").is_err() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let audio = dir.path().join("tone.wav");
    write_wav(&audio);
    let model = dir.path().join("yamnet.onnx");
    fs::write(&model, b"not a model").unwrap();
    fs::write(
        dir.path().join("yamnet_class_map.csv"),
        "index,mid,display_name\n0,/m/09x0r,Speech\n1,/m/04rlf,Music\n",
    )
    .unwrap();
    let mut command = plugin();
    command
        .env("ANYTOPDF_AUDIO_EVENTS_MODEL", &model)
        .env("ORT_DYLIB_PATH", dir.path().join("no-onnxruntime"))
        .env("ANYTOPDF_AUDIO_EVENTS_THRESHOLD", "loud");
    let response = exchange(&mut command, dir.path(), audio_graph(&audio));
    assert_eq!(response["ok"], true, "{response}");
    let warnings = response["warnings"].to_string();
    assert!(
        warnings.contains("ANYTOPDF_AUDIO_EVENTS_THRESHOLD"),
        "{warnings}"
    );
    assert!(warnings.contains("model not used"), "{warnings}");

    let graph: anytopdf_core::DocumentGraph =
        serde_json::from_value(response["graph"].clone()).unwrap();
    graph.validate().unwrap();
    let unit = graph.units.last().unwrap();
    assert_eq!(unit.metadata["audio-events.engine"], "signal");
    let text = unit.visible_text.as_deref().unwrap();
    assert!(
        text.starts_with("Audio events: tone.wav\n\nTimeline: 00:00 music"),
        "{text}"
    );
    let labels: Vec<&str> = unit.annotations.iter().map(|a| a.text.as_str()).collect();
    assert_eq!(labels, ["music", "silence"]);
    assert_eq!(unit.annotations[1].time_range.unwrap().start_seconds, 3.0);
}
