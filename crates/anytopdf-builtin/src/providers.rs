use anytopdf_core::CommandExt;
use std::{path::PathBuf, process::Command, time::Duration};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub const PROVIDER_NAMES: [&str; 10] = [
    "ffmpeg",
    "ffprobe",
    "exiftool",
    "tesseract",
    "python3",
    "soffice",
    "pdftoppm",
    "pdftotext",
    "yt-dlp",
    "chrome",
];

/// yt-dlp for video and podcast URLs: `ANYTOPDF_YT_DLP`, then `yt-dlp` on `PATH`.
pub fn ytdlp_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ANYTOPDF_YT_DLP").filter(|p| !p.is_empty()) {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    which::which("yt-dlp").ok()
}

/// A Chrome-family browser for web page snapshots: `ANYTOPDF_CHROME`, then the usual
/// names on `PATH`, then the standard install locations.
pub fn chrome_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("ANYTOPDF_CHROME").filter(|p| !p.is_empty()) {
        let path = PathBuf::from(path);
        return path.is_file().then_some(path);
    }
    // Vendor builds first: they ship the AppArmor profile Ubuntu 24.04 needs for the
    // browser sandbox, which distribution and downloaded Chromium builds may lack.
    let names = [
        "google-chrome",
        "google-chrome-stable",
        "chrome",
        "msedge",
        "microsoft-edge",
        "chromium",
        "chromium-browser",
    ];
    // Snap-packaged Chromium (Ubuntu's `chromium`) cannot read or write files
    // outside the user's home, so the job workspace in the temp folder is out of its
    // reach; any other install on PATH goes first.
    let found: Vec<PathBuf> = names.iter().filter_map(|n| which::which(n).ok()).collect();
    if let Some(found) = found.iter().find(|p| !is_snap(p)).or_else(|| found.first()) {
        return Some(found.clone());
    }
    let mut known: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        for app in ["Google Chrome", "Chromium", "Microsoft Edge"] {
            known.push(format!("/Applications/{app}.app/Contents/MacOS/{app}").into());
        }
    }
    if cfg!(windows) {
        for var in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var) {
                let base = PathBuf::from(base);
                known.push(base.join(r"Google\Chrome\Application\chrome.exe"));
                known.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
            }
        }
    }
    known.into_iter().find(|p| p.is_file())
}

/// Whether `path` is a snap package or a launcher script that runs one.
fn is_snap(path: &std::path::Path) -> bool {
    if std::fs::canonicalize(path).is_ok_and(|p| p.starts_with("/snap")) {
        return true;
    }
    // Launcher scripts are small; a real browser binary is not read.
    std::fs::metadata(path).is_ok_and(|m| m.len() < 16 * 1024)
        && std::fs::read(path)
            .is_ok_and(|b| b.starts_with(b"#!") && b.windows(6).any(|w| w == b"/snap/"))
}

#[derive(Debug, Clone)]
pub struct ProviderVersion {
    pub name: &'static str,
    pub available: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

/// Providers only URL inputs use; a conversion records their versions only when it
/// used them.
pub const URL_PROVIDERS: [&str; 2] = ["yt-dlp", "chrome"];

pub fn detect_providers() -> Vec<ProviderVersion> {
    detect_providers_named(&PROVIDER_NAMES)
}

/// Like [`detect_providers`], limited to `names` (entries of [`PROVIDER_NAMES`]).
pub fn detect_providers_named(names: &[&'static str]) -> Vec<ProviderVersion> {
    names
        .iter()
        .map(|&name| {
            let path = match name {
                "soffice" => crate::importers::soffice_path(),
                "yt-dlp" => ytdlp_path(),
                "chrome" => chrome_path(),
                _ => which::which(name).ok(),
            };
            let version = path.as_ref().and_then(|p| {
                let output = Command::new(p)
                    .arg(version_flag(name))
                    .bounded_output(PROBE_TIMEOUT)
                    .ok()?;
                parse_version(name, &String::from_utf8_lossy(&output.stdout))
                    .or_else(|| parse_version(name, &String::from_utf8_lossy(&output.stderr)))
            });
            ProviderVersion {
                name,
                available: path.is_some(),
                path,
                version,
            }
        })
        .collect()
}

fn version_flag(name: &str) -> &'static str {
    match name {
        "exiftool" => "-ver",
        "ffmpeg" | "ffprobe" => "-version",
        "pdftoppm" | "pdftotext" => "-v",
        _ => "--version",
    }
}

fn parse_version(name: &str, output: &str) -> Option<String> {
    let mut tokens = output.lines().next()?.split_whitespace();
    let token = match name {
        "ffmpeg" | "ffprobe" => {
            tokens.next().filter(|t| *t == name)?;
            tokens.next().filter(|t| *t == "version")?;
            tokens.next()
        }
        "exiftool" => tokens.next(),
        "tesseract" => tokens
            .next()
            .filter(|t| *t == "tesseract")
            .and(tokens.next()),
        "python3" => tokens.next().filter(|t| *t == "Python").and(tokens.next()),
        "yt-dlp" => tokens.next(),
        // "Chromium 141.0.7390.37", "Google Chrome 141.0…", "Microsoft Edge 141.0…".
        "chrome" => tokens.find(|t| t.starts_with(|c: char| c.is_ascii_digit())),
        "soffice" => tokens
            .next()
            .filter(|t| t.starts_with("LibreOffice"))
            .and(tokens.next()),
        "pdftoppm" | "pdftotext" => {
            tokens.next().filter(|t| *t == name)?;
            tokens.next().filter(|t| *t == "version")?;
            tokens.next()
        }
        _ => None,
    }?;
    token
        .starts_with(|c: char| c.is_ascii_digit())
        .then(|| token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_browsers_and_their_launchers_are_recognised() {
        let dir = tempfile::tempdir().unwrap();
        let launcher = dir.path().join("chromium-browser");
        std::fs::write(&launcher, "#!/bin/sh\nexec /snap/bin/chromium \"$@\"\n").unwrap();
        let real = dir.path().join("chrome");
        std::fs::write(&real, b"\x7fELF not a script").unwrap();
        assert!(is_snap(&launcher));
        assert!(!is_snap(&real));
        assert!(!is_snap(&dir.path().join("missing")));
    }

    #[test]
    fn parses_first_line_versions_of_known_providers() {
        assert_eq!(
            parse_version(
                "ffmpeg",
                "ffmpeg version 7.1.1 Copyright (c) 2000-2025\nbuilt with clang\n"
            )
            .as_deref(),
            Some("7.1.1")
        );
        assert_eq!(
            parse_version("ffprobe", "ffprobe version 7.1.1 Copyright (c) 2007-2025\n").as_deref(),
            Some("7.1.1")
        );
        assert_eq!(
            parse_version("exiftool", "13.25\n").as_deref(),
            Some("13.25")
        );
        assert_eq!(
            parse_version("tesseract", "tesseract 5.5.0\n leptonica-1.85.0\n").as_deref(),
            Some("5.5.0")
        );
        assert_eq!(
            parse_version("python3", "Python 3.11.9\n").as_deref(),
            Some("3.11.9")
        );
        assert_eq!(
            parse_version("soffice", "LibreOffice 24.2.7.2 420(Build:2)\n").as_deref(),
            Some("24.2.7.2")
        );
        assert_eq!(
            parse_version("pdftoppm", "pdftoppm version 24.02.0\nCopyright\n").as_deref(),
            Some("24.02.0")
        );
        assert_eq!(
            parse_version("yt-dlp", "2025.09.26\n").as_deref(),
            Some("2025.09.26")
        );
        assert_eq!(
            parse_version("chrome", "Google Chrome 141.0.7390.54 \n").as_deref(),
            Some("141.0.7390.54")
        );
    }

    #[test]
    fn rejects_unparseable_version_output() {
        for name in PROVIDER_NAMES {
            assert_eq!(parse_version(name, ""), None, "{name} empty");
            assert_eq!(parse_version(name, "garbage"), None, "{name} garbage");
        }
        assert_eq!(parse_version("unknown-tool", "tool 1.2.3\n"), None);
    }

    #[test]
    fn detection_lists_known_providers_in_stable_order() {
        let found = detect_providers();
        let names: Vec<&str> = found.iter().map(|p| p.name).collect();
        assert_eq!(names, PROVIDER_NAMES.to_vec());
        for p in found.iter().filter(|p| !p.available) {
            assert!(p.version.is_none(), "{} version", p.name);
            assert!(p.path.is_none(), "{} path", p.name);
        }
    }
}
