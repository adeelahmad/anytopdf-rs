use std::process::Command;

#[path = "common/process.rs"]
mod process;
use process::command;

fn stdout_of(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn capabilities_without_json_prints_a_table_with_enable_hints() {
    let text = stdout_of(command().arg("capabilities"));
    assert!(
        serde_json::from_str::<serde_json::Value>(&text).is_err(),
        "human output must not be JSON:\n{text}"
    );
    for needle in [
        "Importers:",
        "Enrichers:",
        "Renderers:",
        "OCR providers:",
        "External tools:",
        " AB text ",
        " AB pdf ",
        " -B ffmpeg-video ",
        "runtime plugins: disabled by --no-plugins",
        "Enable missing capabilities:",
        "install ffmpeg:",
        "install tesseract:",
        "Add new capabilities:",
        "anytopdf-plugin-<name>",
    ] {
        assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
    }
}

#[test]
fn capabilities_help_explains_legend_and_adding_plugins() {
    let text = stdout_of(command().args(["capabilities", "--help"]));
    for needle in [
        "A = available",
        "R = runtime plugin",
        "PLUGIN_PROTOCOL.md",
        "ANYTOPDF_PLUGIN_PATH",
        "apple-vision",
        "--json",
    ] {
        assert!(text.contains(needle), "missing `{needle}` in:\n{text}");
    }
}

#[cfg(unix)]
#[test]
fn capabilities_lists_runtime_plugins_from_plugin_path() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let plugin = dir.path().join("anytopdf-plugin-igl");
    std::fs::write(
        &plugin,
        "#!/bin/sh\nprintf '%s' '{\"protocol\":1,\"name\":\"igl\",\"version\":\"0.3.0\",\
         \"capabilities\":[{\"kind\":\"importer\",\"extensions\":[\"igl\"]}]}'\n",
    )
    .unwrap();
    std::fs::set_permissions(&plugin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let text = stdout_of(
        Command::new(env!("CARGO_BIN_EXE_anytopdf"))
            .arg("capabilities")
            .env("PATH", "")
            .env("ANYTOPDF_PLUGIN_PATH", dir.path()),
    );
    assert!(
        text.contains("runtime plugins: 1 found"),
        "plugin not counted:\n{text}"
    );
    let line = text
        .lines()
        .find(|l| l.contains("runtime:igl"))
        .unwrap_or_else(|| panic!("plugin not listed:\n{text}"));
    assert!(line.starts_with(" AR "), "{line}");
    assert!(
        line.contains(".igl") && line.contains("anytopdf-plugin-igl"),
        "{line}"
    );
}
