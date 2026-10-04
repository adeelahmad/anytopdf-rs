use std::{fs, path::Path, process::Command};

fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    command.arg("--no-plugins").env("PATH", "");
    command
}

fn font_available() -> bool {
    if let Some(path) = std::env::var_os("ANYTOPDF_FONT") {
        return Path::new(&path).exists();
    }
    [
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/System/Library/Fonts/Supplemental/Arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
    ]
    .iter()
    .any(|p| Path::new(p).exists())
}

#[test]
fn strict_passes_when_only_optional_provider_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("notes.txt");
    let output = dir.path().join("out.pdf");
    fs::write(&source, "plain ascii notes").unwrap();
    let result = command()
        .arg("convert")
        .arg(&source)
        .args(["--ocr", "off", "--strict", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    let has_font = font_available();
    println!(
        "font branch: {}",
        if has_font {
            "font-present"
        } else {
            "font-absent"
        }
    );
    let info = stderr
        .lines()
        .find(|l| l.contains("INFO [provider.missing]"))
        .unwrap_or_else(|| panic!("no INFO [provider.missing] line in stderr:\n{stderr}"));
    assert!(info.contains("exiftool"), "{info}");
    if has_font {
        assert!(result.status.success(), "{stderr}");
        assert!(output.exists());
    } else {
        for line in stderr.lines().filter(|l| l.contains("WARNING")) {
            assert!(line.contains("[render.warning]"), "{line}");
        }
    }
}

#[test]
fn strict_still_fails_on_skipped_input_with_coded_reason() {
    let dir = tempfile::tempdir().unwrap();
    let valid = dir.path().join("valid.txt");
    let invalid = dir.path().join("invalid.bin");
    let output = dir.path().join("result.pdf");
    fs::write(&valid, "valid text").unwrap();
    fs::write(&invalid, [0u8; 16]).unwrap();
    let result = command()
        .arg("convert")
        .args([&valid, &invalid])
        .args(["--strict", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(!result.status.success(), "{stderr}");
    assert!(!output.exists());
    assert!(stderr.contains("WARNING [input.unsupported]"), "{stderr}");
    assert!(stderr.contains("invalid.bin"), "{stderr}");
}
