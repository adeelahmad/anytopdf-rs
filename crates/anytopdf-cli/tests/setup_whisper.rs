use std::{fs, path::Path, process::Command};

fn anytopdf(data: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    command
        .env("PATH", "")
        .env_remove("ANYTOPDF_PLUGIN_PATH")
        .env_remove("ANYTOPDF_WHISPER_MODEL")
        .env("ANYTOPDF_DATA_DIR", data);
    command
}

#[test]
fn setup_whisper_refuses_a_file_that_does_not_match_the_published_checksum() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let fake = dir.path().join("ggml-tiny.bin");
    fs::write(&fake, b"not a model").unwrap();
    let out = anytopdf(&data)
        .args(["setup", "whisper", "--model", "tiny", "--from"])
        .arg(&fake)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("checksum mismatch"), "{stderr}");
    assert!(!data.join("whisper/ggml-tiny.bin").exists());
    assert!(!data.join("whisper/ggml-tiny.bin.part").exists());
    assert!(!data.join("whisper/setup.json").exists());
}

#[test]
fn setup_whisper_rejects_unknown_models_and_lists_the_choices() {
    let dir = tempfile::tempdir().unwrap();
    let out = anytopdf(dir.path())
        .args(["setup", "whisper", "--model", "enormous"])
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("base.en") && stderr.contains("large-v3-turbo"),
        "{stderr}"
    );
    let help = anytopdf(dir.path())
        .args(["setup", "whisper", "--help"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&help.stdout);
    for needle in [
        "ANYTOPDF_DATA_DIR",
        "SHA-1",
        "whisper-cli",
        "FFmpeg",
        "--from",
    ] {
        assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
    }
}

#[test]
fn doctor_explains_how_to_turn_on_transcription() {
    let dir = tempfile::tempdir().unwrap();
    let out = anytopdf(dir.path()).arg("doctor").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("Transcription:"), "{text}");
    assert!(text.contains("[miss] whisper"), "{text}");
    assert!(text.contains("anytopdf setup whisper"), "{text}");

    let out = anytopdf(dir.path())
        .args(["doctor", "--json"])
        .output()
        .unwrap();
    let doctor: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doctor["transcription"]["available"], false);
    assert!(
        doctor["transcription"]["detail"]
            .as_str()
            .unwrap()
            .contains("anytopdf-plugin-whisper not found")
    );
}

/// Copies anytopdf into a fake install with a `plugins/` folder beside it, the
/// layout release archives and Scoop use.
#[cfg(unix)]
fn install_with_bundled_plugin(root: &Path, manifest: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let bin = root.join("install");
    fs::create_dir_all(bin.join("plugins")).unwrap();
    let exe = bin.join("anytopdf");
    fs::copy(env!("CARGO_BIN_EXE_anytopdf"), &exe).unwrap();
    let staged = root.join("plugin.sh");
    fs::write(&staged, format!("#!/bin/sh\nprintf '%s' '{manifest}'\n")).unwrap();
    let plugin = bin.join("plugins/anytopdf-plugin-whisper");
    // Copy through `cp` so no writable descriptor on the script leaks into a
    // concurrently forked test process (ETXTBSY).
    let copied = Command::new("cp")
        .arg(&staged)
        .arg(&plugin)
        .status()
        .unwrap();
    assert!(copied.success());
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
    exe
}

#[cfg(unix)]
#[test]
fn bundled_whisper_plugin_turns_on_only_when_it_reports_ready() {
    let dir = tempfile::tempdir().unwrap();
    let caps = r#""capabilities":[{"kind":"graph-enricher","mime_types":["audio/*","video/*"]}]"#;
    let run = |exe: &Path, args: &[&str]| {
        let out = Command::new(exe)
            .args(args)
            .env("PATH", "")
            .env_remove("ANYTOPDF_PLUGIN_PATH")
            .env("ANYTOPDF_DATA_DIR", dir.path().join("data"))
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };

    let idle = install_with_bundled_plugin(
        &dir.path().join("idle"),
        &format!(
            r#"{{"protocol":1,"name":"whisper","version":"9.9.9",{caps},"ready":false,"detail":"whisper.cpp has no ggml model; run `anytopdf setup whisper`"}}"#
        ),
    );
    let table = run(&idle, &["capabilities"]);
    assert!(table.contains("runtime plugins: 0 found"), "{table}");
    assert!(table.contains(" -R runtime:whisper"), "{table}");
    assert!(table.contains("turn on runtime:whisper:"), "{table}");
    let plugins = run(&idle, &["plugins"]);
    assert!(!plugins.contains("runtime:whisper"), "{plugins}");
    let doctor = run(&idle, &["doctor"]);
    assert!(
        doctor.contains("[miss] whisper    whisper.cpp has no ggml model"),
        "{doctor}"
    );

    let ready = install_with_bundled_plugin(
        &dir.path().join("ready"),
        &format!(
            r#"{{"protocol":1,"name":"whisper","version":"9.9.9",{caps},"ready":true,"detail":"whisper.cpp:ggml-base via /bin/whisper-cli"}}"#
        ),
    );
    let table = run(&ready, &["capabilities"]);
    assert!(table.contains("runtime plugins: 1 found"), "{table}");
    assert!(table.contains(" AR runtime:whisper"), "{table}");
    let plugins = run(&ready, &["plugins"]);
    assert!(plugins.contains("runtime:whisper"), "{plugins}");
    let doctor = run(&ready, &["doctor"]);
    assert!(
        doctor.contains("[ok]   whisper    whisper.cpp:ggml-base"),
        "{doctor}"
    );
    let disabled = run(&ready, &["--no-plugins", "plugins"]);
    assert!(!disabled.contains("runtime:whisper"), "{disabled}");
}
