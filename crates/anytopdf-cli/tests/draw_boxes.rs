use std::{fs, path::Path, process::Output};

#[path = "common/png.rs"]
mod png;
#[path = "common/png_gray.rs"]
mod png_gray;
use png_gray::write_png;

fn convert(dir: &Path, flags: &[&str]) -> (Output, Option<serde_json::Value>) {
    let image = dir.join("frame.png");
    write_png(&image, 8, 6);
    let graph = dir.join("graph.json");
    let _ = fs::remove_file(&graph);
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", "")
        .arg("convert")
        .args(flags)
        // The input follows the flag so a bare --draw-boxes must not swallow it.
        .arg(&image)
        .args(["--ocr", "off", "--overwrite", "--dump-graph"])
        .arg(&graph)
        .arg("-o")
        .arg(dir.join("out.pdf"))
        .output()
        .unwrap();
    let dump = fs::read(&graph)
        .ok()
        .map(|b| serde_json::from_slice(&b).unwrap());
    (out, dump)
}

fn draw_boxes(dump: &serde_json::Value) -> Option<&str> {
    dump["metadata"]["anytopdf.draw-boxes"].as_str()
}

#[test]
fn draw_boxes_flag_records_the_selected_kinds_for_the_renderer() {
    let dir = tempfile::tempdir().unwrap();
    for (flags, expected) in [
        (&[][..], None),
        (&["--draw-boxes"][..], Some("objects,faces")),
        (&["--draw-boxes=faces,ocr"][..], Some("ocr,faces")),
        (&["--draw-boxes=all"][..], Some("ocr,objects,faces")),
    ] {
        let (out, dump) = convert(dir.path(), flags);
        assert!(
            out.status.success(),
            "{flags:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(draw_boxes(&dump.unwrap()), expected, "{flags:?}");
    }
}

#[test]
fn unknown_draw_boxes_kind_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let (out, _) = convert(dir.path(), &["--draw-boxes=people"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown box kind"), "{stderr}");
}
