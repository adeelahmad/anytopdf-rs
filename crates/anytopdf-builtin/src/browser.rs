//! Rendered page snapshots: headless Chrome, Chromium or Edge printing a page to PDF.

use anyhow::{Context, Result, bail};
use anytopdf_core::{CommandExt, contain_process_tree};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Print `url` to `out` as a PDF with a throwaway browser profile under `scratch`.
/// `offline` sends every network request to a closed local port, so a local page
/// renders without fetching remote images, fonts or scripts.
pub fn print_to_pdf(
    chrome: &Path,
    url: &str,
    out: &Path,
    scratch: &Path,
    timeout: Duration,
    offline: bool,
) -> Result<()> {
    let profile = scratch.join("chrome-profile");
    fs::create_dir_all(&profile)?;
    let mut cmd = Command::new(chrome);
    cmd.args([
        "--headless",
        "--disable-gpu",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-extensions",
        "--disable-sync",
        "--mute-audio",
        "--hide-scrollbars",
        "--no-pdf-header-footer",
        "--print-to-pdf-no-header",
        "--virtual-time-budget=10000",
    ]);
    cmd.arg(format!("--user-data-dir={}", profile.display()));
    cmd.arg(format!("--print-to-pdf={}", out.display()));
    if offline {
        cmd.args([
            "--proxy-server=127.0.0.1:9",
            "--proxy-bypass-list=<-loopback>",
            "--disable-background-networking",
        ]);
    }
    if running_as_root() {
        // Chrome refuses to start its sandbox as root (containers, CI).
        cmd.arg("--no-sandbox");
    }
    cmd.arg(url);
    contain_process_tree(&mut cmd);
    let output = cmd
        .bounded_output_contained(timeout)
        .with_context(|| format!("run {}", chrome.display()))?;
    let _ = fs::remove_dir_all(&profile);
    let head = fs::read(out).ok().map(|b| b.starts_with(b"%PDF"));
    if head != Some(true) {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.lines().rev().find(|l| !l.trim().is_empty());
        bail!(
            "{} did not print the page ({})",
            chrome.display(),
            last.unwrap_or("no output")
        );
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn running_as_root() -> bool {
    use std::os::unix::fs::MetadataExt;
    fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0)
}

#[cfg(not(target_os = "linux"))]
fn running_as_root() -> bool {
    false
}
