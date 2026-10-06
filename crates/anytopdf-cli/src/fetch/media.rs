//! Video and podcast URLs through yt-dlp, with captions saved as a sidecar.

use super::UrlOptions;
use anyhow::{Context, Result, bail};
use anytopdf_core::{CommandExt, TimeRange, contain_process_tree};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Hosts whose pages are players rather than documents, fetched with yt-dlp in auto mode.
const MEDIA_HOSTS: [&str; 14] = [
    "youtube.com",
    "youtu.be",
    "youtube-nocookie.com",
    "vimeo.com",
    "dailymotion.com",
    "twitch.tv",
    "soundcloud.com",
    "podcasts.apple.com",
    "podcasts.google.com",
    "castbox.fm",
    "pca.st",
    "overcast.fm",
    "ted.com",
    "archive.org",
];

pub(super) fn is_media_host(host: &str) -> bool {
    let host = host.to_ascii_lowercase();
    // The Wayback Machine serves archived documents, not media items.
    host != "web.archive.org"
        && MEDIA_HOSTS
            .iter()
            .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// A chapter from the video page, start and end in seconds.
#[derive(Debug, Clone)]
pub(crate) struct Chapter {
    pub(crate) range: TimeRange,
    pub(crate) title: String,
}

pub(super) struct MediaDownload {
    pub(super) path: PathBuf,
    pub(super) metadata: Vec<(String, String)>,
    pub(super) chapters: Vec<Chapter>,
}

pub(super) fn download(
    exe: &Path,
    url: &str,
    dir: &Path,
    opts: &UrlOptions,
) -> Result<MediaDownload> {
    let height = opts.max_height;
    let mut cmd = Command::new(exe);
    cmd.args([
        "--no-playlist",
        "--no-progress",
        "--restrict-filenames",
        "--no-simulate",
        "--print",
        "after_move:filepath",
        "--write-info-json",
        "--write-subs",
        "--write-auto-subs",
        "--sub-format",
        "vtt/srt/best",
        "--sub-langs",
        &opts.sub_langs,
        "-f",
        &format!("bv*[height<={height}]+ba/b[height<={height}]/bv*+ba/b"),
        "--max-filesize",
        &format!("{}M", opts.max_mb),
        "-o",
        "%(title).80B-%(id)s.%(ext)s",
        "-P",
    ]);
    cmd.arg(dir).arg("--").arg(url);
    contain_process_tree(&mut cmd);
    let output = cmd
        .bounded_output_contained(opts.timeout)
        .with_context(|| format!("run {}", exe.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no output");
        bail!("yt-dlp could not fetch {url}: {last}");
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let printed = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .with_context(|| format!("yt-dlp downloaded nothing for {url}"))?;
    let path = PathBuf::from(printed)
        .canonicalize()
        .with_context(|| format!("yt-dlp output {printed}"))?;
    if !path.starts_with(dir.canonicalize()?) || !path.is_file() {
        bail!(
            "yt-dlp wrote {} outside its download directory",
            path.display()
        );
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    attach_captions(&path, &stem, dir)?;
    let (metadata, chapters) = read_info(&dir.join(format!("{stem}.info.json")));
    Ok(MediaDownload {
        path,
        metadata,
        chapters,
    })
}

/// Rename the first downloaded caption (`<stem>.<lang>.vtt`) to `<stem>.vtt` so the
/// sidecar caption enricher attaches it to the media.
fn attach_captions(media: &Path, stem: &str, dir: &Path) -> Result<()> {
    let mut found: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
            name.is_some_and(|n| n.starts_with(&format!("{stem}.")))
                && p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("vtt") || e.eq_ignore_ascii_case("srt"))
        })
        .collect();
    found.sort();
    if let Some(first) = found.first() {
        let ext = first
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        let target = media.with_extension(ext);
        if *first != target {
            fs::rename(first, &target)?;
        }
    }
    Ok(())
}

/// Title, uploader and date from yt-dlp's info JSON as `url.*` source metadata, and
/// the page's chapters.
fn read_info(path: &Path) -> (Vec<(String, String)>, Vec<Chapter>) {
    let Some(info) = fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
    else {
        return (Vec::new(), Vec::new());
    };
    let _ = fs::remove_file(path);
    let mut out = Vec::new();
    for (key, field) in [
        ("url.title", "title"),
        ("url.uploader", "uploader"),
        ("url.channel", "channel"),
        ("url.upload-date", "upload_date"),
        ("url.page", "webpage_url"),
        ("url.extractor", "extractor_key"),
    ] {
        if let Some(value) = info[field].as_str().filter(|v| !v.is_empty()) {
            out.push((key.to_string(), value.to_string()));
        }
    }
    if let Some(duration) = info["duration"].as_f64() {
        out.push(("url.duration".into(), format!("{duration}")));
    }
    let chapters = info["chapters"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let start = c["start_time"].as_f64()?;
            let end = c["end_time"].as_f64()?;
            let title = c["title"].as_str()?.trim();
            (start.is_finite() && end > start && !title.is_empty()).then(|| Chapter {
                range: TimeRange {
                    start_seconds: start,
                    end_seconds: end,
                },
                title: title.to_string(),
            })
        })
        .collect();
    (out, chapters)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_hosts_match_domains_and_subdomains_only() {
        for host in [
            "youtube.com",
            "www.youtube.com",
            "m.youtube.com",
            "youtu.be",
            "podcasts.apple.com",
            "vimeo.com",
        ] {
            assert!(is_media_host(host), "{host}");
        }
        for host in [
            "example.com",
            "notyoutube.com",
            "youtube.com.evil.org",
            "web.archive.org",
        ] {
            assert!(!is_media_host(host), "{host}");
        }
    }

    #[test]
    fn captions_are_renamed_to_the_media_sidecar_name() {
        let tmp = tempfile::tempdir().unwrap();
        let media = tmp.path().join("Talk-abc.mp4");
        fs::write(&media, b"x").unwrap();
        fs::write(tmp.path().join("Talk-abc.en.vtt"), "WEBVTT\n").unwrap();
        fs::write(tmp.path().join("Talk-abc.fr.vtt"), "WEBVTT\n").unwrap();
        attach_captions(&media, "Talk-abc", tmp.path()).unwrap();
        assert!(tmp.path().join("Talk-abc.vtt").is_file());
    }

    #[test]
    fn info_json_becomes_url_metadata_and_is_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("x.info.json");
        fs::write(
            &path,
            r#"{"title":"A talk","uploader":"Someone","upload_date":"20250101","duration":61.5,
                "chapters":[{"start_time":0,"end_time":30,"title":"Intro"},
                            {"start_time":30,"end_time":30,"title":"empty"}]}"#,
        )
        .unwrap();
        let (meta, chapters) = read_info(&path);
        assert_eq!(chapters.len(), 1);
        assert_eq!(chapters[0].title, "Intro");
        assert!(meta.contains(&("url.title".into(), "A talk".into())));
        assert!(meta.contains(&("url.upload-date".into(), "20250101".into())));
        assert!(meta.contains(&("url.duration".into(), "61.5".into())));
        assert!(!path.exists());
    }
}
