use std::{fs, path::Path, process::Command};

use serde_json::Value;

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in data {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend(data);
    out.extend(&body);
    out.extend(crc32(&body).to_be_bytes());
}

/// 4x3 8-bit RGB PNG using a stored (uncompressed) deflate block.
fn write_png(path: &Path) {
    let raw: Vec<u8> = (0..3u8)
        .flat_map(|row| {
            std::iter::once(0u8).chain((0..4u8).flat_map(move |col| [col * 60, row * 80, 200]))
        })
        .collect();
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend((raw.len() as u16).to_le_bytes());
    z.extend((!(raw.len() as u16)).to_le_bytes());
    z.extend(&raw);
    z.extend(((b << 16) | a).to_be_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, b"IHDR", &[0, 0, 0, 4, 0, 0, 0, 3, 8, 2, 0, 0, 0]);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    fs::write(path, png).unwrap();
}

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
    write_png(&source);
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
