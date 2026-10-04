use std::{fs, path::Path, process::Command};

const DIGEST: &str = "eea2ca13a1da285c9365c7dd3fdfb68eb34445313f8eb1b994991728ae3d917a";

fn convert(input: &Path, pdf: &Path, json: &Path) -> serde_json::Value {
    let result = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", "")
        .arg("convert")
        .arg(input)
        .args(["--ocr", "off", "-o"])
        .arg(pdf)
        .arg("--dump-graph")
        .arg(json)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&fs::read(json).unwrap()).unwrap()
}

#[test]
fn repeated_conversion_dumps_identical_ids_and_digest() {
    let dir = tempfile::tempdir().unwrap();
    let input = dir.path().join("notes.txt");
    fs::write(&input, b"Identity fixture\n").unwrap();
    let a = convert(
        &input,
        &dir.path().join("a.pdf"),
        &dir.path().join("a.json"),
    );
    let b = convert(
        &input,
        &dir.path().join("b.pdf"),
        &dir.path().join("b.json"),
    );
    assert_eq!(a["sources"][0]["id"], b["sources"][0]["id"]);
    let (ua, ub) = (
        a["units"].as_array().unwrap(),
        b["units"].as_array().unwrap(),
    );
    assert_eq!(ua.len(), ub.len());
    for (x, y) in ua.iter().zip(ub) {
        assert_eq!(x["id"], y["id"]);
    }
    assert_eq!(a["sources"][0]["sha256"], DIGEST);
    assert_eq!(a["sources"][0]["size"], 17);
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xEDB8_8320 & (!(crc & 1)).wrapping_add(1));
        }
    }
    !crc
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend(data);
    out.extend(&body);
    out.extend(crc32(&body).to_be_bytes());
}

/// A width x height 8-bit grayscale PNG using stored deflate blocks.
fn write_png(path: &Path, width: u32, height: u32) {
    let mut raw = Vec::new();
    for _ in 0..height {
        raw.push(0);
        raw.extend(std::iter::repeat_n(128u8, width as usize));
    }
    let (mut a, mut b) = (1u32, 0u32);
    for byte in &raw {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    let mut z = vec![0x78, 0x01, 0x01];
    z.extend((raw.len() as u16).to_le_bytes());
    z.extend((!(raw.len() as u16)).to_le_bytes());
    z.extend(&raw);
    z.extend(((b << 16) | a).to_be_bytes());
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = Vec::new();
    ihdr.extend(width.to_be_bytes());
    ihdr.extend(height.to_be_bytes());
    ihdr.extend([8, 0, 0, 0, 0]);
    png_chunk(&mut png, b"IHDR", &ihdr);
    png_chunk(&mut png, b"IDAT", &z);
    png_chunk(&mut png, b"IEND", &[]);
    fs::write(path, png).unwrap();
}

#[test]
fn graph_dump_units_carry_kind_appropriate_anchors() {
    let dir = tempfile::tempdir().unwrap();
    let text = dir.path().join("notes.txt");
    fs::write(&text, b"Identity fixture\n").unwrap();
    let image = dir.path().join("image.png");
    write_png(&image, 4, 3);
    let dump = convert_all(&[&text, &image], dir.path());
    let units = dump["units"].as_array().unwrap();
    assert_eq!(units.len(), 2);
    for unit in units {
        assert!(unit.get("anchor").is_some(), "unit without anchor: {unit}");
    }
    let by_kind = |kind: &str| units.iter().find(|u| u["kind"] == kind).unwrap();
    let anchor = &by_kind("text")["anchor"];
    assert_eq!(anchor["kind"], "byte-range");
    assert_eq!(anchor["start"], 0);
    assert_eq!(anchor["end"], 17);
    let anchor = &by_kind("visual")["anchor"];
    assert_eq!(anchor["kind"], "region");
    assert_eq!(anchor["width"], 1.0);
    assert_eq!(anchor["height"], 1.0);
}

fn convert_all(inputs: &[&Path], dir: &Path) -> serde_json::Value {
    let json = dir.join("all.json");
    let result = Command::new(env!("CARGO_BIN_EXE_anytopdf"))
        .arg("--no-plugins")
        .env("PATH", "")
        .arg("convert")
        .args(inputs)
        .args(["--ocr", "off", "-o"])
        .arg(dir.join("all.pdf"))
        .arg("--dump-graph")
        .arg(&json)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&fs::read(json).unwrap()).unwrap()
}
