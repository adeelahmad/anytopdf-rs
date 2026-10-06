//! Plain HTTP(S) downloads with checked redirects and a size limit.

use super::guard;
use anyhow::{Context, Result, bail};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use ureq::http::Uri;
use ureq::tls::{Certificate, RootCerts, TlsConfig};

const MAX_REDIRECTS: usize = 10;

pub(super) struct Download {
    pub(super) path: PathBuf,
    pub(super) content_type: String,
    pub(super) final_url: String,
}

impl Download {
    pub(super) fn is_html(&self) -> bool {
        matches!(
            self.content_type.as_str(),
            "text/html" | "application/xhtml+xml"
        )
    }
}

pub(super) fn agent(timeout: Duration) -> ureq::Agent {
    let native: Vec<Certificate<'static>> = rustls_native_certs::load_native_certs()
        .certs
        .iter()
        .map(|c| Certificate::from_der(c.as_ref()).to_owned())
        .collect();
    let roots = if native.is_empty() {
        RootCerts::WebPki
    } else {
        RootCerts::new_with_certs(&native)
    };
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(0)
        .user_agent(concat!("anytopdf/", env!("CARGO_PKG_VERSION")))
        .tls_config(TlsConfig::builder().root_certs(roots).build())
        .build()
        .into()
}

/// Download `url` into `dir` as `<stem>.<ext>`, following and checking each redirect.
pub(super) fn download(
    agent: &ureq::Agent,
    url: &str,
    dir: &Path,
    stem: &str,
    max_bytes: u64,
    allow_private: bool,
) -> Result<Download> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let uri: Uri = current
            .parse()
            .with_context(|| format!("invalid URL: {current}"))?;
        guard::check(&uri, allow_private)?;
        let mut response = agent
            .get(&current)
            .header(
                "accept",
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .call()
            .with_context(|| format!("fetch {current}"))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .with_context(|| format!("{current} redirected without a Location"))?;
            current = join(&uri, location);
            continue;
        }
        if !status.is_success() {
            bail!("fetch {current}: HTTP {}", status.as_u16());
        }
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                v.split(';')
                    .next()
                    .unwrap_or_default()
                    .trim()
                    .to_ascii_lowercase()
            })
            .unwrap_or_default();
        let ext = extension_for(&uri, &content_type);
        let path = dir.join(format!("{stem}.{ext}"));
        let mut file =
            fs::File::create(&path).with_context(|| format!("create {}", path.display()))?;
        let mut reader = response.body_mut().with_config().limit(max_bytes).reader();
        io::copy(&mut reader, &mut file).with_context(|| {
            format!(
                "download {current} (limit {} MiB)",
                max_bytes / (1024 * 1024)
            )
        })?;
        return Ok(Download {
            path,
            content_type,
            final_url: current,
        });
    }
    bail!("fetch {url}: more than {MAX_REDIRECTS} redirects")
}

/// Resolve a redirect `Location` against the URL that returned it.
pub(super) fn join(base: &Uri, location: &str) -> String {
    let lower = location.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return location.to_string();
    }
    let scheme = base.scheme_str().unwrap_or("https");
    if let Some(rest) = location.strip_prefix("//") {
        return format!("{scheme}://{rest}");
    }
    let authority = base.authority().map(|a| a.as_str()).unwrap_or_default();
    if location.starts_with('/') {
        return format!("{scheme}://{authority}{location}");
    }
    let path = base.path();
    let dir = &path[..path.rfind('/').map_or(0, |i| i + 1)];
    let dir = if dir.is_empty() { "/" } else { dir };
    format!("{scheme}://{authority}{dir}{location}")
}

/// File extension for a download: the URL's own when it has a short one, else one for
/// the content type, so importers can probe by extension as well as by magic bytes.
pub(super) fn extension_for(uri: &Uri, content_type: &str) -> String {
    if matches!(content_type, "text/html" | "application/xhtml+xml") {
        return "html".into();
    }
    let from_path = uri
        .path()
        .rsplit('/')
        .next()
        .and_then(|name| name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .filter(|ext| {
            (1..=5).contains(&ext.len()) && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        });
    if let Some(ext) = from_path {
        return ext;
    }
    let ext = match content_type {
        "application/pdf" => "pdf",
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/tiff" => "tiff",
        "image/heic" => "heic",
        "image/avif" => "avif",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" | "audio/m4a" => "m4a",
        "audio/aac" => "aac",
        "audio/ogg" => "ogg",
        "audio/opus" => "opus",
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/flac" | "audio/x-flac" => "flac",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "text/plain" => "txt",
        "text/markdown" => "md",
        "text/vtt" => "vtt",
        "application/x-subrip" => "srt",
        "message/rfc822" => "eml",
        "application/zip" => "zip",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        _ => "bin",
    };
    ext.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_locations_resolve_against_the_base() {
        let base: Uri = "https://example.com/a/b/page?x=1".parse().unwrap();
        assert_eq!(join(&base, "https://other.org/x"), "https://other.org/x");
        assert_eq!(
            join(&base, "//cdn.example.com/y"),
            "https://cdn.example.com/y"
        );
        assert_eq!(join(&base, "/root"), "https://example.com/root");
        assert_eq!(
            join(&base, "next.html"),
            "https://example.com/a/b/next.html"
        );
        let bare: Uri = "http://example.com".parse().unwrap();
        assert_eq!(join(&bare, "next"), "http://example.com/next");
    }

    #[test]
    fn extensions_prefer_the_url_then_the_content_type() {
        let uri = |s: &str| s.parse::<Uri>().unwrap();
        assert_eq!(extension_for(&uri("https://x.org/a/ep1.MP3"), ""), "mp3");
        assert_eq!(
            extension_for(&uri("https://x.org/feed"), "audio/mpeg"),
            "mp3"
        );
        assert_eq!(
            extension_for(&uri("https://x.org/doc.php"), "text/html"),
            "html"
        );
        assert_eq!(
            extension_for(&uri("https://x.org/a.b/c"), "application/pdf"),
            "pdf"
        );
        assert_eq!(
            extension_for(&uri("https://x.org/download"), "x/unknown"),
            "bin"
        );
    }
}
