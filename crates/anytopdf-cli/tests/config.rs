use std::{fs, path::Path, process::Command};

use serde_json::{Value, json};

#[path = "common/schema_assert.rs"]
mod schema_assert;
#[path = "common/schema.rs"]
mod schema_files;
#[path = "common/stdout_json.rs"]
mod stdout_json;
use schema_assert::assert_valid;
use stdout_json::single_document;

/// The CLI with no user config file and no inherited ANYTOPDF_* settings.
fn anytopdf(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_anytopdf"));
    for (name, _) in std::env::vars_os() {
        if name.to_string_lossy().starts_with("ANYTOPDF_") {
            command.env_remove(name);
        }
    }
    command
        .env("PATH", "")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("APPDATA", home.join("AppData"));
    command
}

fn stdout_of(command: &mut Command) -> Vec<u8> {
    let out = command.output().unwrap();
    assert!(
        out.status.success(),
        "{command:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
}

#[test]
fn config_json_reports_every_layer_and_matches_the_schemas() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("anytopdf.toml");
    fs::write(
        &file,
        "profile = \"share\"\n[importer.video]\ninterval = 2.0\n[enricher.whisper]\nmodel = \"base\"\n",
    )
    .unwrap();
    let report = single_document(&stdout_of(
        anytopdf(dir.path())
            .env("ANYTOPDF_IMPORTER__VIDEO__SCENE_THRESHOLD", "0.5")
            .arg("--config")
            .arg(&file)
            .args(["--plugin-timeout", "9", "--set", "enricher.ocr.lang=deu"])
            .args(["config", "--json"]),
    ));
    assert_valid("config", &report);
    assert_valid("config-file", &report["values"]);
    let values = &report["values"];
    assert_eq!(values["profile"], "share");
    assert_eq!(values["plugin_timeout"], 9);
    assert_eq!(values["importer"]["video"]["interval"], 2.0);
    assert_eq!(values["importer"]["video"]["scene_threshold"], 0.5);
    assert_eq!(values["importer"]["video"]["max_frames"], 0);
    assert_eq!(values["enricher"]["ocr"]["lang"], "deu");
    assert_eq!(values["enricher"]["whisper"]["model"], "base");
    let origins = &report["origins"];
    assert_eq!(origins["profile"]["kind"], "file");
    assert_eq!(
        origins["plugin_timeout"],
        json!({"kind": "flag", "flag": "--plugin-timeout"})
    );
    assert_eq!(
        origins["importer.video.scene_threshold"],
        json!({"kind": "env", "name": "ANYTOPDF_IMPORTER__VIDEO__SCENE_THRESHOLD"})
    );
    assert_eq!(origins["enricher.ocr.lang"], json!({"kind": "set"}));
    assert_eq!(
        origins["importer.video.max_frames"],
        json!({"kind": "default"})
    );
    assert_eq!(report["files"][0]["loaded"], true);
    assert_eq!(report["files"][0]["explicit"], true);
}

#[test]
fn the_user_config_file_applies_unless_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let user = if cfg!(windows) {
        dir.path().join("AppData").join("anytopdf")
    } else {
        dir.path().join(".config").join("anytopdf")
    };
    fs::create_dir_all(&user).unwrap();
    fs::write(user.join("config.toml"), "strict = true\n").unwrap();
    let report = single_document(&stdout_of(anytopdf(dir.path()).args(["config", "--json"])));
    assert_eq!(report["values"]["strict"], true);
    assert_eq!(report["files"][0]["explicit"], false);

    let report = single_document(&stdout_of(anytopdf(dir.path()).args([
        "--no-config",
        "config",
        "--json",
    ])));
    assert_eq!(report["values"]["strict"], false);
    assert_eq!(report["files"], json!([]));
}

#[test]
fn the_defaults_template_is_a_valid_config_file() {
    let dir = tempfile::tempdir().unwrap();
    let template = stdout_of(anytopdf(dir.path()).args(["config", "--defaults"]));
    let file = dir.path().join("defaults.toml");
    fs::write(&file, &template).unwrap();
    let from_file = single_document(&stdout_of(
        anytopdf(dir.path())
            .arg("--config")
            .arg(&file)
            .args(["config", "--json"]),
    ));
    let defaults = single_document(&stdout_of(anytopdf(dir.path()).args([
        "--no-config",
        "config",
        "--json",
    ])));
    assert_eq!(from_file["values"], defaults["values"]);
    assert!(
        from_file["origins"]
            .as_object()
            .unwrap()
            .values()
            .all(|origin| origin["kind"] == "file"),
        "{}",
        from_file["origins"]
    );
}

#[test]
fn an_invalid_setting_exits_2_naming_the_key_and_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("bad.toml");
    fs::write(&file, "[importer.video]\ninterval = \"fast\"\n").unwrap();
    let out = anytopdf(dir.path())
        .arg("--config")
        .arg(&file)
        .args(["convert", "a.txt"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("importer.video.interval") && stderr.contains("bad.toml"),
        "{stderr}"
    );
}

#[test]
fn config_settings_reach_convert_and_flags_override_them() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("anytopdf.toml");
    fs::write(
        &file,
        "profile = \"share\"\nplugins = false\n[enricher.ocr]\nmode = \"off\"\n",
    )
    .unwrap();
    let note = dir.path().join("note.txt");
    fs::write(&note, "configured").unwrap();
    let convert = |extra: &[&str], output: &str| -> Value {
        single_document(&stdout_of(
            anytopdf(dir.path())
                .arg("--config")
                .arg(&file)
                .arg("convert")
                .arg(&note)
                .args(extra)
                .arg("-o")
                .arg(dir.path().join(output))
                .arg("--json"),
        ))
    };
    assert_eq!(convert(&[], "a.pdf")["profile"], "share");
    assert_eq!(
        convert(&["--profile", "archive"], "b.pdf")["profile"],
        "archive"
    );
}

#[cfg(unix)]
#[test]
fn runtime_plugins_receive_their_table() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    fs::create_dir(&plugins).unwrap();
    let seen = dir.path().join("request.json");
    let staged = dir.path().join("plugin.sh");
    fs::write(
        &staged,
        format!(
            r#"#!/bin/sh
if [ "$1" = --anytopdf-manifest ]; then
  echo '{{"protocol":1,"name":"cfg-probe","version":"1","capabilities":[{{"kind":"importer","extensions":["cfgp"],"priority":90}}]}}'
  exit 0
fi
cp "$2" '{}'
printf '%s' '{{"protocol":1,"ok":true,"units":[{{"kind":"text","visible_text":"configured plugin"}}]}}' > "$4"
"#,
            seen.display()
        ),
    )
    .unwrap();
    // Copy through `cp` so no concurrently forked test inherits a writable
    // descriptor on the executable (ETXTBSY).
    let plugin = plugins.join("anytopdf-plugin-cfg-probe");
    assert!(
        Command::new("cp")
            .arg(&staged)
            .arg(&plugin)
            .status()
            .unwrap()
            .success()
    );
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
    let source = dir.path().join("input.cfgp");
    fs::write(&source, "input").unwrap();
    stdout_of(
        anytopdf(dir.path())
            .env("PATH", "/usr/bin:/bin")
            .env("ANYTOPDF_PLUGIN_PATH", &plugins)
            .env("ANYTOPDF_IMPORTER__CFG_PROBE__DEPTH", "3")
            .args(["--set", "importer.cfg-probe.layers=[\"walls\"]", "convert"])
            .arg(&source)
            .args(["--ocr", "off", "-o"])
            .arg(dir.path().join("out.pdf")),
    );
    let request: Value = serde_json::from_slice(&fs::read(&seen).unwrap()).unwrap();
    assert_eq!(request["options"], json!({"depth": 3, "layers": ["walls"]}));
}
