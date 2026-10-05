use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    command.arg("--no-plugins").env("PATH", "");
    command
}

fn run(args: &[&std::ffi::OsStr]) -> Output {
    command().arg("convert").args(args).output().unwrap()
}

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

/// 4x3 8-bit grayscale PNG using a stored (uncompressed) deflate block.
fn write_gray_ramp_png(path: &Path) {
    let raw: Vec<u8> = (0..3).flat_map(|_| [0u8, 10, 100, 200, 250]).collect();
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
    chunk(&mut png, b"IHDR", &[0, 0, 0, 4, 0, 0, 0, 3, 8, 0, 0, 0, 0]);
    chunk(&mut png, b"IDAT", &z);
    chunk(&mut png, b"IEND", &[]);
    fs::write(path, png).unwrap();
}

#[test]
fn successful_conversion_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("notes.txt");
    let output = dir.path().join("out.pdf");
    fs::write(&source, "plain notes").unwrap();
    let result = run(&[
        source.as_ref(),
        "--ocr".as_ref(),
        "off".as_ref(),
        "-o".as_ref(),
        output.as_ref(),
    ]);
    assert_eq!(
        result.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(output.exists());
}

#[test]
fn invalid_option_value_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("notes.txt");
    let output = dir.path().join("out.pdf");
    fs::write(&source, "plain notes").unwrap();
    let result = run(&[
        source.as_ref(),
        "--video-interval".as_ref(),
        "0".as_ref(),
        "-o".as_ref(),
        output.as_ref(),
    ]);
    assert_eq!(
        result.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!output.exists());
}

#[test]
fn missing_input_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("absent.txt");
    let result = run(&[absent.as_ref()]);
    assert_eq!(
        result.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn unavailable_explicit_ocr_provider_exits_4() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("pixel.png");
    let output = dir.path().join("out.pdf");
    write_gray_ramp_png(&source);
    let result = run(&[
        source.as_ref(),
        "--ocr".as_ref(),
        "tesseract".as_ref(),
        "-o".as_ref(),
        output.as_ref(),
    ]);
    let stderr = String::from_utf8_lossy(&result.stderr).to_lowercase();
    assert_eq!(result.status.code(), Some(4), "{stderr}");
    assert!(!output.exists());
    assert!(stderr.contains("tesseract"), "{stderr}");
}

#[test]
fn strict_warning_exits_5_without_publishing() {
    let dir = tempfile::tempdir().unwrap();
    let valid = dir.path().join("valid.txt");
    let invalid = dir.path().join("invalid.bin");
    let output = dir.path().join("out.pdf");
    fs::write(&valid, "valid text").unwrap();
    fs::write(&invalid, [0u8; 16]).unwrap();
    let result = run(&[
        valid.as_ref(),
        invalid.as_ref(),
        "--strict".as_ref(),
        "-o".as_ref(),
        output.as_ref(),
    ]);
    assert_eq!(
        result.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!output.exists());
}

#[test]
fn render_failure_exits_6_without_publishing() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("notes.txt");
    let font = dir.path().join("fake.ttf");
    let output = dir.path().join("out.pdf");
    fs::write(&source, "plain notes").unwrap();
    fs::write(&font, [0x41u8; 64]).unwrap();
    let result = command()
        .env("ANYTOPDF_FONT", &font)
        .arg("convert")
        .arg(&source)
        .args(["--ocr", "off", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(6),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!output.exists());
}

#[test]
fn gray_ramp_png_fixture_bytes_are_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ramp.png");
    write_gray_ramp_png(&path);
    assert_eq!(
        anytopdf_core::sha256_hex(&fs::read(&path).unwrap()),
        "bd6a4c5dc04a58bb63fd728dc2d41498fa4dfa2fd4c612ea1a39efc27be2e041"
    );
}
