#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

#[path = "common/plugin_host.rs"]
mod plugin_host;
use plugin_host::anytopdf_with_plugins as command;

/// Install a plugin whose import leaves a background process behind that writes
/// `marker` a second after the call returns.
fn install_leaky_plugin(dir: &Path, marker: &Path) -> std::path::PathBuf {
    let plugins = dir.join("plugins");
    fs::create_dir(&plugins).unwrap();
    let staged = dir.join("plugin.sh");
    fs::write(
        &staged,
        format!(
            r#"#!/bin/sh
if [ "$1" = --anytopdf-manifest ]; then
  echo '{{"protocol":1,"name":"leaky","version":"1","capabilities":[{{"kind":"importer","extensions":["leaky"],"priority":90}}]}}'
  exit 0
fi
( /bin/sleep 1; echo leaked > '{}' ) </dev/null >/dev/null 2>&1 &
printf '%s' '{{"protocol":1,"ok":true,"units":[{{"kind":"text","visible_text":"queued"}}]}}' > "$4"
"#,
            marker.display()
        ),
    )
    .unwrap();
    // Copy through `cp` so no concurrently forked test inherits a writable
    // descriptor on the executable (ETXTBSY).
    let plugin = plugins.join("anytopdf-plugin-leaky");
    assert!(
        std::process::Command::new("cp")
            .arg(&staged)
            .arg(&plugin)
            .status()
            .unwrap()
            .success()
    );
    fs::set_permissions(&plugin, fs::Permissions::from_mode(0o755)).unwrap();
    plugins
}

/// Queue one `.leaky` job and run a single worker pass with `global` flags.
fn run_job(dir: &Path, plugins: &Path, global: &[&str]) {
    fs::write(dir.join("input.leaky"), "input").unwrap();
    for sub in [
        &["add", "q", "input.leaky", "--", "--ocr", "off"][..],
        &["work", "q", "--once"],
    ] {
        let out = command()
            .env("ANYTOPDF_PLUGIN_PATH", plugins)
            .args(global)
            .arg("queue")
            .args(sub)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{sub:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(dir.join("q/outbox/input.pdf").is_file());
}

fn appears_within(path: &Path, limit: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < limit {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    path.exists()
}

#[test]
fn queued_jobs_contain_plugin_processes_by_default() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("leaked");
    let plugins = install_leaky_plugin(tmp.path(), &marker);
    run_job(tmp.path(), &plugins, &[]);
    assert!(
        !appears_within(&marker, Duration::from_secs(3)),
        "a plugin's background process outlived its queued job"
    );
}

#[test]
fn queue_sandbox_default_can_be_turned_off() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("leaked");
    let plugins = install_leaky_plugin(tmp.path(), &marker);
    run_job(tmp.path(), &plugins, &["--plugin-sandbox", "off"]);
    assert!(
        appears_within(&marker, Duration::from_secs(10)),
        "with --plugin-sandbox off the background process should have run"
    );
}
