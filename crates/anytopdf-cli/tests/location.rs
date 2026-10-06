use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn convert(inputs: &[&Path], dir: &Path, extra: &[&str], path_env: &str) -> (Value, PathBuf) {
    let pdf = dir.join(format!("out-{}.pdf", extra.join("-")));
    let graph = dir.join(format!("graph-{}.json", extra.join("-")));
    let out = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", path_env)
        .arg("convert")
        .args(inputs)
        .args(["--ocr", "off", "--overwrite", "-o"])
        .arg(&pdf)
        .arg("--dump-graph")
        .arg(&graph)
        .args(extra)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (
        serde_json::from_slice(&fs::read(&graph).unwrap()).unwrap(),
        pdf,
    )
}

fn locations(graph: &Value) -> Vec<&Value> {
    graph["units"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|u| u["annotations"].as_array().unwrap())
        .filter(|a| a["kind"] == "location")
        .collect()
}

fn notes(dir: &Path) -> PathBuf {
    let path = dir.join("trip.txt");
    fs::write(&path, "Day one in San Francisco, then a flight to Tokyo.\n").unwrap();
    path
}

#[test]
fn place_names_in_text_become_location_annotations() {
    let dir = tempfile::tempdir().unwrap();
    let source = notes(dir.path());
    let (graph, pdf) = convert(&[&source], dir.path(), &[], "");
    let found = locations(&graph);
    let places: Vec<&str> = found.iter().map(|a| a["text"].as_str().unwrap()).collect();
    assert_eq!(
        places,
        ["San Francisco, California, United States", "Tokyo, Japan"],
        "{found:?}"
    );
    assert!(found.iter().all(|a| a["attributes"]["source"] == "text"));
    assert!(found.iter().all(|a| a["provider"] == "location"));

    let Ok(pdftotext) = which::which("pdftotext") else {
        eprintln!("POPPLER-UNAVAILABLE: pdftotext not found; hidden place names not checked");
        return;
    };
    let text = Command::new(pdftotext).arg(&pdf).arg("-").output().unwrap();
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("San Francisco, California, United States"),
        "{text}"
    );
}

#[test]
fn location_off_and_gps_skip_text_place_names() {
    let dir = tempfile::tempdir().unwrap();
    let source = notes(dir.path());
    for mode in ["off", "gps"] {
        let (graph, _) = convert(&[&source], dir.path(), &["--location", mode], "");
        assert!(locations(&graph).is_empty(), "{mode}: {graph}");
    }
}

#[test]
fn share_profile_keeps_text_place_names() {
    let dir = tempfile::tempdir().unwrap();
    let source = notes(dir.path());
    let (graph, _) = convert(&[&source], dir.path(), &["--profile", "share"], "");
    assert_eq!(locations(&graph).len(), 2, "{graph}");
}

#[test]
fn video_gps_tag_is_reverse_geocoded_and_dropped_by_share() {
    let (Ok(ffmpeg), Ok(_)) = (which::which("ffmpeg"), which::which("ffprobe")) else {
        eprintln!("FFMPEG-UNAVAILABLE: video GPS location not exercised");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let video = dir.path().join("walk.mp4");
    let made = Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=64x48:d=1",
        ])
        .args(["-metadata", "location=+48.8584+002.2945/"])
        .args(["-pix_fmt", "yuv420p", "-y"])
        .arg(&video)
        .output()
        .unwrap();
    assert!(
        made.status.success(),
        "{}",
        String::from_utf8_lossy(&made.stderr)
    );
    let path_env = std::env::var("PATH").unwrap_or_default();

    let (graph, pdf) = convert(&[&video], dir.path(), &[], &path_env);
    let found = locations(&graph);
    assert_eq!(found.len(), 1, "{found:?}");
    let gps = found[0];
    assert_eq!(gps["attributes"]["source"], "gps");
    assert_eq!(gps["attributes"]["country_code"], "FR");
    assert_eq!(gps["attributes"]["latitude"], "48.858400");
    assert!(gps["text"].as_str().unwrap().ends_with("France"), "{gps}");
    if let Ok(pdftotext) = which::which("pdftotext") {
        let text = Command::new(pdftotext).arg(&pdf).arg("-").output().unwrap();
        let text = String::from_utf8_lossy(&text.stdout);
        assert!(
            !text.contains("France"),
            "GPS place in hidden layer: {text}"
        );
    }

    let (shared, _) = convert(&[&video], dir.path(), &["--profile", "share"], &path_env);
    assert!(locations(&shared).is_empty(), "{shared}");
}
