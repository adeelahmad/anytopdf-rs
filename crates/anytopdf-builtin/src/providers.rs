use anytopdf_core::CommandExt;
use std::{path::PathBuf, process::Command, time::Duration};

const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

pub const PROVIDER_NAMES: [&str; 8] = [
    "ffmpeg",
    "ffprobe",
    "exiftool",
    "tesseract",
    "python3",
    "soffice",
    "pdftoppm",
    "pdftotext",
];

#[derive(Debug, Clone)]
pub struct ProviderVersion {
    pub name: &'static str,
    pub available: bool,
    pub path: Option<PathBuf>,
    pub version: Option<String>,
}

pub fn detect_providers() -> Vec<ProviderVersion> {
    PROVIDER_NAMES
        .iter()
        .map(|&name| {
            let path = match name {
                "soffice" => crate::importers::soffice_path(),
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
