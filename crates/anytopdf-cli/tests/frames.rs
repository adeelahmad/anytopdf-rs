use std::{fs, path::Path, process::Command};

use anytopdf_core::{Uuid, content_unit_id};
use serde_json::Value;

#[path = "common/png.rs"]
mod png;
#[path = "common/png_rgb.rs"]
mod png_rgb;
#[path = "common/schema_assert.rs"]
mod schema_assert;
#[path = "common/schema.rs"]
mod schema_files;
use png_rgb::write_rgb_png;
use schema_assert::assert_valid;

fn anytopdf() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    command
        .env("PATH", "")
        .env("SOURCE_DATE_EPOCH", "1700000000");
    command
}

fn has_frame_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.contains_key("frame") || map.values().any(has_frame_key),
        Value::Array(items) => items.iter().any(has_frame_key),
        _ => false,
    }
}

#[test]
fn single_frame_png_manifest_matches_the_2f6f36e_golden() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("one.png");
    let output = dir.path().join("out.pdf");
    write_rgb_png(&source);
    let convert = anytopdf()
        .args(["--no-plugins", "convert"])
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .args(["--profile", "share", "--ocr", "off"])
        .output()
        .unwrap();
    assert_eq!(
        convert.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&convert.stderr)
    );
    let extract = anytopdf()
        .args(["--no-plugins", "extract"])
        .arg(&output)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(extract.status.code(), Some(0));
    let extracted: Value = serde_json::from_slice(&extract.stdout).unwrap();
    let manifest = &extracted["manifest"];
    let golden: Value =
        serde_json::from_str(include_str!("golden/single_frame_png_manifest.json")).unwrap();
    assert_eq!(manifest, &golden);
    assert!(
        !has_frame_key(&manifest["units"]),
        "no anchor carries frame"
    );
}

fn write_tiff(path: &Path, values: &[u8]) {
    const IFD_LEN: usize = 2 + 9 * 12 + 4;
    let pixels_at = (8 + IFD_LEN * values.len()) as u32;
    let mut out = vec![b'I', b'I', 42, 0, 8, 0, 0, 0];
    for i in 0..values.len() {
        out.extend_from_slice(&9u16.to_le_bytes());
        for (tag, kind, value) in [
            (256u16, 3u16, 1u32),
            (257, 3, 1),
            (258, 3, 8),
            (259, 3, 1),
            (262, 3, 1),
            (273, 4, pixels_at + i as u32),
            (277, 3, 1),
            (278, 3, 1),
            (279, 4, 1),
        ] {
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&kind.to_le_bytes());
            out.extend_from_slice(&1u32.to_le_bytes());
            out.extend_from_slice(&value.to_le_bytes());
        }
        let next = if i + 1 < values.len() {
            (8 + IFD_LEN * (i + 1)) as u32
        } else {
            0
        };
        out.extend_from_slice(&next.to_le_bytes());
    }
    out.extend_from_slice(values);
    fs::write(path, out).unwrap();
}

/// 2x2 GIF whose frames are solid palette indices 0 and 1 (red 0 and 100).
fn write_gif(path: &Path) {
    let mut out = b"GIF89a".to_vec();
    out.extend([2, 0, 2, 0, 0b1000_0001, 0, 0]);
    out.extend([0, 0, 0, 100, 0, 0, 0, 0, 0, 0, 0, 0]);
    for index in [0u32, 1] {
        out.extend([0x2C, 0, 0, 0, 0, 2, 0, 2, 0, 0, 2]);
        // 3-bit codes: clear, index, clear, index, clear, index, clear, index, end.
        let codes = [4, index, 4, index, 4, index, 4, index, 5];
        let (mut bits, mut count) = (0u64, 0u32);
        let mut data = Vec::new();
        for code in codes {
            bits |= u64::from(code) << count;
            count += 3;
            while count >= 8 {
                data.push((bits & 0xFF) as u8);
                bits >>= 8;
                count -= 8;
            }
        }
        if count > 0 {
            data.push(bits as u8);
        }
        out.push(data.len() as u8);
        out.extend(data);
        out.push(0);
    }
    out.push(0x3B);
    fs::write(path, out).unwrap();
}

fn run(args: &[&str], source: &Path, output: &Path) -> std::process::Output {
    anytopdf()
        .args(["--no-plugins", "convert"])
        .arg(source)
        .arg("-o")
        .arg(output)
        .args(args)
        .output()
        .unwrap()
}

fn extract(pdf: &Path) -> Value {
    let out = anytopdf()
        .args(["--no-plugins", "extract"])
        .arg(pdf)
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn json_stdout(out: &std::process::Output) -> Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("json: {e}: {}", String::from_utf8_lossy(&out.stderr)))
}

fn has_code(payload: &Value, code: &str) -> bool {
    payload["diagnostics"]
        .as_array()
        .is_some_and(|d| d.iter().any(|x| x["code"] == code))
}

fn assert_frame_units(manifest: &Value, frames: usize) {
    let units = manifest["units"].as_array().unwrap();
    assert_eq!(units.len(), frames);
    for (k, unit) in units.iter().enumerate() {
        assert_eq!(unit["anchor"]["kind"], "region");
        assert_eq!(unit["anchor"]["frame"], k as u64);
        assert_eq!(unit["pages"]["first"], (k + 1) as u64);
        assert_eq!(unit["pages"]["last"], (k + 1) as u64);
    }
}

#[test]
fn three_page_tiff_converts_to_three_pages_with_frame_anchors() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("scan.tif");
    let output = dir.path().join("out.pdf");
    write_tiff(&source, &[10, 20, 30]);
    let convert = run(&["--no-provenance-page", "--json"], &source, &output);
    assert_eq!(convert.status.code(), Some(0));
    let payload = json_stdout(&convert);
    assert_eq!(payload["outputs"][0]["pages"], 3);
    assert!(!has_code(&payload, "input.frames-not-imported"));
    let extracted = extract(&output);
    assert_valid("extract", &extracted);
    assert_valid("manifest", &extracted["manifest"]);
    assert_valid("chunks", &extracted["chunks"]);
    let manifest = &extracted["manifest"];
    assert_frame_units(manifest, 3);
    let source_id: Uuid = manifest["sources"][0]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    for (k, unit) in manifest["units"].as_array().unwrap().iter().enumerate() {
        assert_eq!(unit["id"], content_unit_id(source_id, k).to_string());
    }
}

#[test]
fn two_frame_gif_converts_to_two_pages() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("anim.gif");
    let output = dir.path().join("out.pdf");
    write_gif(&source);
    let convert = run(&["--no-provenance-page", "--json"], &source, &output);
    assert_eq!(convert.status.code(), Some(0));
    let payload = json_stdout(&convert);
    assert_eq!(payload["outputs"][0]["pages"], 2);
    assert!(!has_code(&payload, "input.frames-not-imported"));
    assert_frame_units(&extract(&output)["manifest"], 2);
}

#[test]
fn max_image_frames_caps_import_and_strict_fails() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("scan.tif");
    let capped = dir.path().join("capped.pdf");
    let strict = dir.path().join("strict.pdf");
    write_tiff(&source, &[10, 20, 30]);
    let convert = run(
        &["--max-image-frames", "2", "--json", "--no-provenance-page"],
        &source,
        &capped,
    );
    assert_eq!(convert.status.code(), Some(0));
    let payload = json_stdout(&convert);
    assert_eq!(payload["outputs"][0]["pages"], 2);
    let warned = payload["diagnostics"].as_array().unwrap().iter().any(|d| {
        d["code"] == "input.frames-not-imported"
            && d["message"]
                .as_str()
                .is_some_and(|m| m.contains("imported 2 of 3"))
    });
    assert!(warned, "{payload}");
    let failed = run(
        &[
            "--max-image-frames",
            "2",
            "--strict",
            "--json",
            "--no-provenance-page",
        ],
        &source,
        &strict,
    );
    assert_eq!(failed.status.code(), Some(5));
    assert!(!strict.exists());
}

#[test]
fn multi_frame_conversion_is_byte_reproducible() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("scan.tif");
    let (a, b) = (dir.path().join("a.pdf"), dir.path().join("b.pdf"));
    write_tiff(&source, &[10, 20, 30]);
    for out in [&a, &b] {
        assert_eq!(run(&["--json"], &source, out).status.code(), Some(0));
    }
    assert_eq!(fs::read(&a).unwrap(), fs::read(&b).unwrap());
    assert_eq!(extract(&a)["manifest"], extract(&b)["manifest"]);
    let convert = run(&["--json"], &source, &dir.path().join("c.pdf"));
    assert_eq!(json_stdout(&convert)["outputs"][0]["pages"], 4);
}

#[test]
fn rgb_png_fixture_bytes_are_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rgb.png");
    write_rgb_png(&path);
    assert_eq!(
        anytopdf_core::sha256_hex(&fs::read(&path).unwrap()),
        "c8186323045c7bb6bfc1655931ae1a9eebd6e2fcfda0be148300b9d0e191c8bf"
    );
}
