use anytopdf_core::{DocumentGraph, RuntimePluginManifest, RuntimeResponse};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    process::Command,
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const SOURCE: &str = "11111111-1111-4111-8111-111111111111";

type Route = dyn Fn(&str, &Value) -> (u16, Value) + Send + Sync;

/// A minimal HTTP/1.1 server that records each JSON request body.
struct Mock {
    url: String,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Mock {
    fn start(
        delay: Duration,
        route: impl Fn(&str, &Value) -> (u16, Value) + Send + Sync + 'static,
    ) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = requests.clone();
        let route: Arc<Route> = Arc::new(route);
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (log, route) = (log.clone(), route.clone());
                thread::spawn(move || serve(stream, delay, &log, &*route));
            }
        });
        Mock { url, requests }
    }

    fn requests(&self) -> Vec<(String, Value)> {
        self.requests.lock().unwrap().clone()
    }
}

fn serve(stream: TcpStream, delay: Duration, log: &Mutex<Vec<(String, Value)>>, route: &Route) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let path = line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .trim_start_matches("/v1/")
        .to_string();
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).unwrap();
        if header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap();
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    log.lock().unwrap().push((path.clone(), body.clone()));
    thread::sleep(delay);
    let (status, reply) = route(&path, &body);
    let reply = reply.to_string();
    let mut stream = stream;
    let _ = write!(
        stream,
        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
        reply.len()
    );
}

/// OpenAI-style answers that echo the prompt they were given.
fn chat(_: &str, body: &Value) -> (u16, Value) {
    let content = &body["messages"][0]["content"];
    let prompt = content
        .as_str()
        .or_else(|| content[0]["text"].as_str())
        .unwrap_or_default();
    let answer = if prompt.starts_with("Classify") {
        "Education".to_string()
    } else if prompt.starts_with("List up to") {
        "1. Fractions\n2. Classroom teaching".to_string()
    } else if prompt.contains("one scene") {
        "  A scene\nsummary. ".to_string()
    } else if prompt.contains("one video") {
        "A teacher explains fractions at a whiteboard.".to_string()
    } else {
        format!("answer to: {prompt}")
    };
    (
        200,
        json!({"choices": [{"message": {"role": "assistant", "content": answer}}]}),
    )
}

fn plugin(vars: &[(&str, &str)]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf-plugin-vlm"));
    for (name, _) in std::env::vars() {
        if name.starts_with("ANYTOPDF_VLM_") || name.starts_with("ANYTOPDF_LLM_") {
            command.env_remove(name);
        }
    }
    command.envs(vars.iter().copied());
    command
}

fn manifest(vars: &[(&str, &str)]) -> RuntimePluginManifest {
    let output = plugin(vars).arg("--anytopdf-manifest").output().unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn exchange(mut command: Command, workspace: &Path, request: Value) -> Value {
    let request_path = workspace.join("request.json");
    let response_path = workspace.join("response.json");
    let mut request = request;
    request["protocol"] = json!(1);
    request["workspace"] = json!(workspace);
    request["future_request_field"] = json!(true);
    fs::write(&request_path, serde_json::to_vec(&request).unwrap()).unwrap();
    let status = command
        .arg("--anytopdf-request")
        .arg(&request_path)
        .arg("--anytopdf-response")
        .arg(&response_path)
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!workspace.join("response.json.tmp").exists());
    let raw = fs::read(&response_path).unwrap();
    // The host must accept every response.
    serde_json::from_slice::<RuntimeResponse>(&raw).unwrap();
    serde_json::from_slice(&raw).unwrap()
}

fn keyframe(dir: &Path, name: &str) -> String {
    let path = dir.join(name);
    image::RgbImage::from_pixel(64, 48, image::Rgb([20, 120, 200]))
        .save(&path)
        .unwrap();
    path.to_string_lossy().into_owned()
}

fn visual_unit(id: &str, path: &str, seconds: f64, selection: &str) -> Value {
    json!({
        "id": id, "source_id": SOURCE, "kind": "visual", "visual_path": path,
        "visible_text": null, "time_range": {"start_seconds": seconds, "end_seconds": seconds},
        "annotations": [], "metadata": {"video.frame-selection": selection}
    })
}

fn unit_request(unit: Value) -> Value {
    json!({"operation": "unit-enrich", "unit": unit, "graph": null, "output": null,
           "source": {"id": SOURCE, "path": "/videos/lesson.mp4", "detected_type": "video/mp4", "metadata": {}}})
}

#[test]
fn manifest_is_inert_until_an_endpoint_is_configured() {
    let inert = manifest(&[]);
    assert_eq!((inert.protocol, inert.name.as_str()), (1, "vlm"));
    assert!(inert.capabilities.is_empty());

    let full = manifest(&[("ANYTOPDF_VLM_URL", "http://127.0.0.1:9/v1")]);
    let kinds: Vec<_> = full.capabilities.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(kinds, ["unit-enricher", "graph-enricher"]);
    assert_eq!(full.capabilities[0].mime_types, ["image/*", "video/*"]);
    assert_eq!(full.capabilities[1].phase.as_deref(), Some("after-units"));

    let moondream = manifest(&[
        ("ANYTOPDF_VLM_URL", "http://127.0.0.1:9/v1"),
        ("ANYTOPDF_VLM_ENGINE", "moondream"),
    ]);
    assert_eq!(moondream.capabilities.len(), 1);

    // A typo still registers the enricher, so the error reaches the user.
    let broken = manifest(&[
        ("ANYTOPDF_VLM_URL", "http://127.0.0.1:9/v1"),
        ("ANYTOPDF_VLM_ENGINE", "gpt"),
    ]);
    assert_eq!(broken.capabilities.len(), 1);
    let dir = tempfile::tempdir().unwrap();
    let frame = keyframe(dir.path(), "f.png");
    let response = exchange(
        plugin(&[
            ("ANYTOPDF_VLM_URL", "http://127.0.0.1:9/v1"),
            ("ANYTOPDF_VLM_ENGINE", "gpt"),
        ]),
        dir.path(),
        unit_request(visual_unit(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            &frame,
            1.0,
            "interval",
        )),
    );
    assert_eq!(response["ok"], false);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("ANYTOPDF_VLM_ENGINE")
    );
}

#[test]
fn keyframes_get_one_caption_per_prompt() {
    let mock = Mock::start(Duration::ZERO, chat);
    let dir = tempfile::tempdir().unwrap();
    let frame = keyframe(dir.path(), "frame.png");
    let response = exchange(
        plugin(&[
            ("ANYTOPDF_VLM_URL", &mock.url),
            ("ANYTOPDF_VLM_MODEL", "llava"),
        ]),
        dir.path(),
        unit_request(visual_unit(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            &frame,
            12.5,
            "interval",
        )),
    );
    assert_eq!(response["ok"], true, "{response}");
    assert_eq!(response["warnings"], json!([]));
    assert!(
        response.get("unit").is_none(),
        "annotations are append-only"
    );
    let annotations = response["annotations"].as_array().unwrap();
    let types: Vec<_> = annotations
        .iter()
        .map(|a| a["attributes"]["caption_type"].as_str().unwrap())
        .collect();
    assert_eq!(types, ["caption", "query", "activity"]);
    for a in annotations {
        assert_eq!(a["kind"], "caption");
        assert_eq!(a["provider"], "vlm:llava");
        assert_eq!(a["time_range"]["start_seconds"], 12.5);
        assert_eq!(
            a["text"],
            format!("answer to: {}", a["attributes"]["prompt"].as_str().unwrap())
        );
    }
    assert_eq!(
        annotations[1]["attributes"]["prompt"],
        "What do you see in this image?"
    );

    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    let (path, body) = &requests[0];
    assert_eq!(path, "chat/completions");
    assert_eq!(body["model"], "llava");
    let image = body["messages"][0]["content"][1]["image_url"]["url"]
        .as_str()
        .unwrap();
    assert!(image.starts_with("data:image/jpeg;base64,"));
}

#[test]
fn moondream_engine_uses_caption_query_and_detect() {
    let mock = Mock::start(Duration::ZERO, |path, body| match path {
        "caption" => (200, json!({"caption": "A red car parked by a wall."})),
        "query" => (
            200,
            json!({"answer": format!("Q: {}", body["question"].as_str().unwrap())}),
        ),
        "detect" => (
            200,
            json!({"objects": [
                {"x_min": 0.25, "y_min": 0.5, "x_max": 0.75, "y_max": 1.2},
                {"x_min": 0.3, "y_min": 0.3, "x_max": 0.3, "y_max": 0.4}
            ]}),
        ),
        _ => (404, json!({})),
    });
    let dir = tempfile::tempdir().unwrap();
    let frame = keyframe(dir.path(), "photo.jpg");
    let response = exchange(
        plugin(&[
            ("ANYTOPDF_VLM_URL", &mock.url),
            ("ANYTOPDF_VLM_ENGINE", "moondream"),
            ("ANYTOPDF_VLM_PROMPTS", "caption=x|Is there a logo?"),
            ("ANYTOPDF_VLM_DETECT", "red car"),
        ]),
        dir.path(),
        unit_request(json!({
            "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "source_id": SOURCE, "kind": "visual",
            "visual_path": frame, "visible_text": null, "time_range": null,
            "annotations": [], "metadata": {}
        })),
    );
    assert_eq!(response["warnings"], json!([]));
    let annotations = response["annotations"].as_array().unwrap();
    assert_eq!(annotations.len(), 3, "{annotations:?}");
    assert_eq!(annotations[0]["text"], "A red car parked by a wall.");
    assert_eq!(annotations[1]["text"], "Q: Is there a logo?");
    assert_eq!(annotations[1]["attributes"]["caption_type"], "query");
    let object = &annotations[2];
    assert_eq!(
        (object["kind"].as_str(), object["text"].as_str()),
        (Some("object"), Some("red car"))
    );
    // Clamped to the frame; the zero-width box is dropped.
    assert_eq!(
        object["region"],
        json!({"x": 0.25, "y": 0.5, "width": 0.5, "height": 0.5})
    );
    let paths: Vec<_> = mock.requests().into_iter().map(|(p, _)| p).collect();
    assert_eq!(paths, ["caption", "query", "detect"]);
}

#[test]
fn unreachable_endpoint_is_one_warning_not_a_failure() {
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let dir = tempfile::tempdir().unwrap();
    let frame = keyframe(dir.path(), "frame.png");
    let response = exchange(
        plugin(&[("ANYTOPDF_VLM_URL", &format!("http://127.0.0.1:{port}/v1"))]),
        dir.path(),
        unit_request(visual_unit(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            &frame,
            0.0,
            "interval",
        )),
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["annotations"], json!([]));
    let warnings = response["warnings"].as_array().unwrap();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0]
            .as_str()
            .unwrap()
            .starts_with("vlm: frame.png: caption:")
    );
}

#[test]
fn slow_frames_stop_at_the_time_budget() {
    let mock = Mock::start(Duration::from_secs(5), chat);
    let dir = tempfile::tempdir().unwrap();
    let frame = keyframe(dir.path(), "frame.png");
    let started = Instant::now();
    let response = exchange(
        plugin(&[
            ("ANYTOPDF_VLM_URL", &mock.url),
            ("ANYTOPDF_VLM_TIMEOUT", "1"),
        ]),
        dir.path(),
        unit_request(visual_unit(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            &frame,
            0.0,
            "interval",
        )),
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["annotations"], json!([]));
    assert_eq!(response["warnings"].as_array().unwrap().len(), 1);
}

#[test]
fn text_units_and_unreadable_images_are_skipped() {
    let mock = Mock::start(Duration::ZERO, chat);
    let dir = tempfile::tempdir().unwrap();
    let vars = [("ANYTOPDF_VLM_URL", mock.url.as_str())];
    let text = exchange(
        plugin(&vars),
        dir.path(),
        unit_request(json!({
            "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "source_id": SOURCE, "kind": "text",
            "visual_path": null, "visible_text": "hello", "time_range": null,
            "annotations": [], "metadata": {}
        })),
    );
    assert_eq!(
        (text["annotations"].clone(), text["warnings"].clone()),
        (json!([]), json!([]))
    );
    let broken = dir.path().join("broken.png");
    fs::write(&broken, b"not a png").unwrap();
    let response = exchange(
        plugin(&vars),
        dir.path(),
        unit_request(visual_unit(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            broken.to_str().unwrap(),
            0.0,
            "interval",
        )),
    );
    assert_eq!(response["ok"], true);
    assert_eq!(response["warnings"].as_array().unwrap().len(), 1);
    assert!(mock.requests().is_empty());
}

fn caption(text: &str, kind: &str, seconds: f64) -> Value {
    json!({"kind": "caption", "text": text, "provider": "vlm:llava", "confidence": null,
           "region": null, "time_range": {"start_seconds": seconds, "end_seconds": seconds},
           "attributes": {"caption_type": kind, "prompt": "p"}})
}

fn video_graph(dir: &Path) -> Value {
    let mut units = vec![
        visual_unit(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1",
            &keyframe(dir, "1.png"),
            0.0,
            "interval",
        ),
        visual_unit(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa2",
            &keyframe(dir, "2.png"),
            5.0,
            "interval",
        ),
        visual_unit(
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa3",
            &keyframe(dir, "3.png"),
            9.0,
            "scene",
        ),
    ];
    units[0]["annotations"] = json!([caption("A whiteboard with fractions.", "caption", 0.0)]);
    units[1]["annotations"] = json!([caption("A teacher points at 1/2.", "activity", 5.0)]);
    units[2]["annotations"] = json!([caption("Students raise hands.", "caption", 9.0)]);
    units.push(json!({
        "id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "source_id": SOURCE, "kind": "text",
        "visual_path": null, "visible_text": "[00:00:01.000 --> 00:00:03.000] Today: fractions.",
        "time_range": null, "metadata": {},
        "annotations": [{"kind": "transcript", "text": "Today: fractions.", "provider": "whisper",
                         "confidence": null, "region": null,
                         "time_range": {"start_seconds": 1.0, "end_seconds": 3.0}, "attributes": {}}]
    }));
    json!({
        "sources": [{"id": SOURCE, "path": dir.join("lesson.mp4"), "detected_type": "video/mp4", "metadata": {}}],
        "units": units,
        "metadata": {}
    })
}

#[test]
fn videos_get_a_summary_page_scene_summaries_and_a_category() {
    let mock = Mock::start(Duration::ZERO, chat);
    let dir = tempfile::tempdir().unwrap();
    let response = exchange(
        plugin(&[
            ("ANYTOPDF_VLM_URL", &mock.url),
            ("ANYTOPDF_VLM_MODEL", "llava"),
            ("ANYTOPDF_LLM_MODEL", "llama3.2"),
        ]),
        dir.path(),
        json!({"operation": "graph-enrich", "graph": video_graph(dir.path()), "unit": null, "source": null, "output": null}),
    );
    assert_eq!(response["warnings"], json!([]), "{response}");
    let graph: DocumentGraph = serde_json::from_value(response["graph"].clone()).unwrap();
    graph.validate().unwrap();

    let units = response["graph"]["units"].as_array().unwrap();
    assert_eq!(units.len(), 5);
    let summary = &units[0];
    assert_eq!(summary["kind"], "text");
    assert_eq!(summary["metadata"]["vlm.summary"], "source");
    assert_eq!(
        summary["visible_text"],
        "Video summary\n\nA teacher explains fractions at a whiteboard.\n\nCategory: education\n\nTopics: Fractions, Classroom teaching"
    );
    assert_eq!(summary["annotations"][0]["kind"], "custom");
    assert_eq!(summary["annotations"][0]["text"], "education");
    assert_eq!(
        summary["annotations"][0]["attributes"]["entity"],
        "category"
    );
    assert_eq!(summary["annotations"][0]["provider"], "vlm:llama3.2");
    let topics: Vec<_> = summary["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["attributes"]["entity"] == "topic")
        .map(|a| a["text"].as_str().unwrap())
        .collect();
    assert_eq!(topics, ["Fractions", "Classroom teaching"]);

    let scene = |unit: &Value| {
        unit["annotations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["attributes"]["caption_type"] == "scene-summary")
            .cloned()
    };
    let first = scene(&units[1]).unwrap();
    assert_eq!(first["text"], "A scene summary.");
    assert_eq!(first["attributes"]["scene"], "1");
    assert_eq!(
        first["time_range"],
        json!({"start_seconds": 0.0, "end_seconds": 9.0})
    );
    assert!(scene(&units[2]).is_none());
    assert_eq!(scene(&units[3]).unwrap()["attributes"]["scene"], "2");

    let requests = mock.requests();
    assert_eq!(requests.len(), 5, "video, category, topics, two scenes");
    let video_prompt = requests[0].1["messages"][0]["content"].as_str().unwrap();
    assert_eq!(requests[0].1["model"], "llama3.2");
    assert!(video_prompt.contains("[00:00:05] A teacher points at 1/2."));
    assert!(video_prompt.contains("[00:00:01] Today: fractions."));
    assert!(video_prompt.contains("Do not guess anyone's age"));
    let scene_two = requests[4].1["messages"][0]["content"].as_str().unwrap();
    assert!(scene_two.contains("Students raise hands.") && !scene_two.contains("whiteboard"));

    // A second run over the enriched graph changes nothing.
    let again = exchange(
        plugin(&[("ANYTOPDF_VLM_URL", &mock.url)]),
        dir.path(),
        json!({"operation": "graph-enrich", "graph": response["graph"], "unit": null, "source": null, "output": null}),
    );
    assert!(again.get("graph").is_none());
    assert_eq!(mock.requests().len(), 5);
}

#[test]
fn audio_and_subtitle_sources_are_summarized_from_their_transcript() {
    let mock = Mock::start(Duration::ZERO, chat);
    let dir = tempfile::tempdir().unwrap();
    let mut graph = video_graph(dir.path());
    graph["sources"][0]["detected_type"] = json!("audio/mpeg");
    // An audio source: a placeholder unit plus Whisper's transcript unit.
    let transcript = graph["units"][3].clone();
    graph["units"] = json!([{
        "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1", "source_id": SOURCE, "kind": "audio",
        "visual_path": null, "visible_text": "Audio source", "time_range": null,
        "annotations": [], "metadata": {}
    }, transcript]);
    let response = exchange(
        plugin(&[("ANYTOPDF_VLM_URL", &mock.url)]),
        dir.path(),
        json!({"operation": "graph-enrich", "graph": graph, "unit": null, "source": null, "output": null}),
    );
    assert_eq!(response["warnings"], json!([]), "{response}");
    serde_json::from_value::<DocumentGraph>(response["graph"].clone())
        .unwrap()
        .validate()
        .unwrap();
    let summary = &response["graph"]["units"][0];
    assert!(
        summary["visible_text"]
            .as_str()
            .unwrap()
            .starts_with("Summary\n\n")
    );
    let prompt = mock.requests()[0].1["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(prompt.starts_with("Below is the transcript of one recording."));
    assert!(
        prompt.contains("[00:00:01] Today: fractions.")
            && !prompt.contains("Keyframe descriptions:\n[")
    );
    assert_eq!(mock.requests().len(), 3, "summary, category, topics");

    // A subtitle file: the importer's cues are captions with time ranges.
    let mut subtitles = video_graph(dir.path());
    subtitles["sources"][0]["detected_type"] = json!("application/x-subrip");
    let mut cue = transcript["annotations"][0].clone();
    cue["kind"] = json!("caption");
    cue["provider"] = json!("subtitle");
    subtitles["units"] = json!([{
        "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1", "source_id": SOURCE, "kind": "text",
        "visible_text": "Today: fractions.", "visual_path": null, "time_range": null,
        "annotations": [cue], "metadata": {}
    }]);
    let response = exchange(
        plugin(&[("ANYTOPDF_VLM_URL", &mock.url)]),
        dir.path(),
        json!({"operation": "graph-enrich", "graph": subtitles, "unit": null, "source": null, "output": null}),
    );
    assert_eq!(response["graph"]["units"].as_array().unwrap().len(), 2);
}

#[test]
fn graphs_without_descriptions_or_video_are_left_alone() {
    let mock = Mock::start(Duration::ZERO, chat);
    let dir = tempfile::tempdir().unwrap();
    let mut graph = video_graph(dir.path());
    graph["units"] = json!([visual_unit(
        "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaa1",
        &keyframe(dir.path(), "1.png"),
        0.0,
        "interval"
    )]);
    let vars = [("ANYTOPDF_VLM_URL", mock.url.as_str())];
    let bare = exchange(
        plugin(&vars),
        dir.path(),
        json!({"operation": "graph-enrich", "graph": graph, "unit": null, "source": null, "output": null}),
    );
    assert!(bare.get("graph").is_none());
    let mut photo = video_graph(dir.path());
    photo["sources"][0]["detected_type"] = json!("image/png");
    // Keyframe descriptions alone do not summarize a non-video source.
    photo["units"].as_array_mut().unwrap().truncate(3);
    let photo = exchange(
        plugin(&vars),
        dir.path(),
        json!({"operation": "graph-enrich", "graph": photo, "unit": null, "source": null, "output": null}),
    );
    assert!(photo.get("graph").is_none());
    assert!(mock.requests().is_empty());
}
