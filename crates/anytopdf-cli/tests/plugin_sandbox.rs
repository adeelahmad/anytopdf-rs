use std::fs;

#[path = "common/plugin_host.rs"]
mod plugin_host;
use plugin_host::anytopdf_with_plugins as command;

#[test]
fn allow_read_without_strict_sandbox_exits_2() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("notes.txt");
    fs::write(&source, "plain notes").unwrap();
    let result = command()
        .arg("--plugin-sandbox-allow-read")
        .arg(dir.path())
        .arg("convert")
        .arg(&source)
        .args(["--ocr", "off", "-o"])
        .arg(dir.path().join("out.pdf"))
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&result.stderr).contains("requires --plugin-sandbox strict"));
}

#[cfg(windows)]
#[test]
fn strict_sandbox_is_refused_where_it_cannot_be_enforced() {
    let result = command()
        .args(["--plugin-sandbox", "strict", "plugins"])
        .output()
        .unwrap();
    assert_eq!(result.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn strict_sandbox_runs_plugin_but_blocks_writes_outside_workspace() {
    use std::os::unix::fs::PermissionsExt;
    if !cfg!(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )) {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let plugins = dir.path().join("plugins");
    fs::create_dir(&plugins).unwrap();
    let plugin = plugins.join("anytopdf-plugin-sandboxed");
    let escape = outside.path().join("escaped");
    // Copy through `cp` so no concurrently forked test inherits a writable
    // descriptor on the executable (ETXTBSY), as `external.rs` tests do.
    let staged = dir.path().join("plugin.sh");
    fs::write(
        &staged,
        format!(
            r#"#!/bin/sh
if [ "$1" = --anytopdf-manifest ]; then
  echo '{{"protocol":1,"name":"sandboxed","version":"1","capabilities":[{{"kind":"importer","extensions":["boxed"],"priority":90}}]}}'
  exit 0
fi
echo escaped > '{}' 2>/dev/null
printf '%s' '{{"protocol":1,"ok":true,"units":[{{"kind":"text","visible_text":"inside the box"}}]}}' > "$4"
"#,
            escape.display()
        ),
    )
    .unwrap();
    let copied = std::process::Command::new("cp")
        .arg(&staged)
        .arg(&plugin)
        .status()
        .unwrap();
    assert!(copied.success());
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
    let source = dir.path().join("input.boxed");
    fs::write(&source, "input").unwrap();
    let output = dir.path().join("out.pdf");
    let result = command()
        .env("ANYTOPDF_PLUGIN_PATH", &plugins)
        .args(["--plugin-sandbox", "strict", "convert"])
        .arg(&source)
        .args(["--ocr", "off", "-o"])
        .arg(&output)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&result.stderr);
    if result.status.code() == Some(2) && stderr.contains("strict plugin sandbox needs") {
        eprintln!("skipping: {stderr}");
        return;
    }
    assert_eq!(result.status.code(), Some(0), "{stderr}");
    assert!(output.exists());
    assert!(!escape.exists(), "plugin wrote outside the workspace");
}
