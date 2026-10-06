//! http(s) inputs: pages, plain files and yt-dlp media, served from a local test server.

#[path = "common/process.rs"]
mod process;

use process::command;
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const POST: &str = "<html><head><title>Field notes</title></head><body>\
    <nav>Home Archive About</nav><main><h1>Field notes</h1>\
    <p>The heron returned on Tuesday.</p></main><footer>Copyright site</footer></body></html>";

/// Serve a few fixed routes on loopback; returns the base URL and a request counter.
fn serve() -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let mut line = String::new();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            reader.read_line(&mut line).unwrap_or_default();
            loop {
                let mut header = String::new();
                if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                    break;
                }
            }
            let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (status, extra, kind, body) = match path.as_str() {
                "/old" => (
                    "301 Moved Permanently",
                    "Location: /post\r\n",
                    "text/plain",
                    "",
                ),
                "/post" => ("200 OK", "", "text/html; charset=utf-8", POST),
                "/notes" => (
                    "200 OK",
                    "",
                    "text/plain",
                    "Remember the milk and the heron.\n",
                ),
                _ => ("404 Not Found", "", "text/plain", "missing"),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\n{extra}Content-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, hits)
}

fn convert(args: &[&str], dir: &Path) -> (Command, std::path::PathBuf) {
    let out = dir.join("out.pdf");
    let mut cmd = convert_default_name(args, dir);
    cmd.arg("-o").arg(&out);
    (cmd, out)
}

/// `convert` without `-o`, isolated from proxies and URL settings in the environment.
fn convert_default_name(args: &[&str], dir: &Path) -> Command {
    let mut cmd = command();
    cmd.current_dir(dir)
        .args(["convert", "--ocr", "off", "--url-snapshot", "off"])
        .args(args);
    for var in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
        "ANYTOPDF_URL_ALLOW_PRIVATE",
        "ANYTOPDF_URL_MODE",
    ] {
        cmd.env_remove(var);
    }
    cmd
}

fn run(mut cmd: Command) -> Output {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "exit {:?}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn extract(pdf: &Path) -> Value {
    let out = run({
        let mut cmd = command();
        cmd.arg("extract").arg(pdf).arg("--json");
        cmd
    });
    serde_json::from_slice(&out.stdout).unwrap()
}

fn chunk_text(doc: &Value) -> String {
    doc["chunks"]["chunks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn web_page_url_is_fetched_through_redirects_and_records_its_source() {
    let (base, _) = serve();
    let tmp = tempfile::tempdir().unwrap();
    let url = format!("{base}/old");
    let (mut cmd, pdf) = convert(&["--url-allow-private", &url], tmp.path());
    cmd.arg("--quiet");
    run(cmd);
    let doc = extract(&pdf);
    let source = &doc["manifest"]["sources"][0];
    let meta = &source["metadata"];
    assert_eq!(meta["url.source"], url.as_str());
    assert_eq!(meta["url.final"], format!("{base}/post").as_str());
    assert_eq!(meta["url.kind"], "page");
    assert_eq!(meta["html.content"], "main");
    assert!(
        meta["url.fetched"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z'))
    );
    let text = chunk_text(&doc);
    assert!(text.contains("The heron returned on Tuesday."), "{text}");
    assert!(!text.contains("Home Archive About"), "{text}");
}

#[test]
fn default_output_is_named_after_the_url() {
    let (base, _) = serve();
    let tmp = tempfile::tempdir().unwrap();
    let url = format!("{base}/post");
    run(convert_default_name(
        &["--url-allow-private", "--quiet", &url],
        tmp.path(),
    ));
    assert!(tmp.path().join("127.0.0.1-post.pdf").is_file());
}

#[test]
fn private_urls_are_refused_before_any_request() {
    let (base, hits) = serve();
    let tmp = tempfile::tempdir().unwrap();
    let (mut cmd, pdf) = convert(&[&format!("{base}/post")], tmp.path());
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("--url-allow-private"), "{stderr}");
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    assert!(!pdf.exists());
}

#[test]
fn non_html_urls_are_imported_by_content_type() {
    let (base, _) = serve();
    let tmp = tempfile::tempdir().unwrap();
    let (cmd, pdf) = convert(
        &["--url-allow-private", "--quiet", &format!("{base}/notes")],
        tmp.path(),
    );
    run(cmd);
    let doc = extract(&pdf);
    let meta = &doc["manifest"]["sources"][0]["metadata"];
    assert_eq!(meta["url.kind"], "file");
    assert_eq!(meta["url.content-type"], "text/plain");
    assert!(chunk_text(&doc).contains("Remember the milk"));
}

#[test]
fn failed_downloads_exit_with_the_input_class() {
    let (base, _) = serve();
    let tmp = tempfile::tempdir().unwrap();
    let (mut cmd, _) = convert(
        &["--url-allow-private", &format!("{base}/gone")],
        tmp.path(),
    );
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("HTTP 404"));
}

#[test]
fn media_urls_without_ytdlp_exit_with_the_provider_class() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut cmd, _) = convert(&["https://www.youtube.com/watch?v=abc"], tmp.path());
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(4));
    assert!(String::from_utf8_lossy(&out.stderr).contains("needs yt-dlp"));
}

#[cfg(unix)]
#[test]
fn media_urls_use_ytdlp_and_attach_its_captions() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    let fake = bin.join("yt-dlp");
    // Stands in for yt-dlp: writes audio, an English caption and info JSON to -P DIR.
    std::fs::write(
        &fake,
        r#"#!/bin/sh
dir=""
while [ $# -gt 0 ]; do
  if [ "$1" = "-P" ]; then dir="$2"; shift; fi
  shift
done
printf 'ID3' > "$dir/Talk-x1.mp3"
printf 'WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nThe heron speaks\n' > "$dir/Talk-x1.en.vtt"
printf '{"title":"Talk","uploader":"Bird Club"}' > "$dir/Talk-x1.info.json"
echo "$dir/Talk-x1.mp3"
"#,
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (mut cmd, pdf) = convert(
        &["--quiet", "https://podcasts.apple.com/us/podcast/x/id1"],
        tmp.path(),
    );
    cmd.env("ANYTOPDF_YT_DLP", &fake);
    run(cmd);
    let doc = extract(&pdf);
    let meta = &doc["manifest"]["sources"][0]["metadata"];
    assert_eq!(meta["url.kind"], "media");
    assert_eq!(meta["url.title"], "Talk");
    assert_eq!(meta["url.uploader"], "Bird Club");
    assert!(chunk_text(&doc).contains("The heron speaks"));
}

#[test]
fn bookmark_exports_convert_every_link_and_skip_unreachable_ones() {
    let (base, _) = serve();
    let tmp = tempfile::tempdir().unwrap();
    let list = tmp.path().join("bookmarks.html");
    std::fs::write(
        &list,
        format!(
            "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<DL><p>\n\
             <DT><H3>Reading</H3>\n<DL><p>\n\
             <DT><A HREF=\"{base}/post\">Heron post</A>\n\
             <DT><A HREF=\"{base}/gone\">Dead link</A>\n\
             </DL><p>\n<DT><A HREF=\"{base}/notes\">Notes</A>\n</DL><p>\n"
        ),
    )
    .unwrap();
    let out = run(convert_default_name(
        &["--url-allow-private", "--json", "--links", "bookmarks.html"],
        tmp.path(),
    ));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["status"], "partial", "{report}");
    let skipped = report["summary"]["skipped"].to_string();
    assert!(skipped.contains("/gone") && skipped.contains("HTTP 404"), "{skipped}");
    let doc = extract(&tmp.path().join("bookmarks.pdf"));
    let sources = doc["manifest"]["sources"].as_array().unwrap();
    assert_eq!(sources.len(), 2);
    let meta = &sources[0]["metadata"];
    assert_eq!(meta["outline.title"], "Heron post");
    assert_eq!(meta["outline.folders"], "Reading");
    assert_eq!(meta["url.list"], "bookmarks.html");
    assert_eq!(sources[1]["metadata"]["outline.title"], "Notes");
    assert!(sources[1]["metadata"].get("outline.folders").is_none());
}

#[test]
fn plain_text_link_lists_need_no_other_inputs() {
    let (base, _) = serve();
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("reading.txt"),
        format!("# my list\n{base}/notes\n{base}/post Field notes\n"),
    )
    .unwrap();
    let (cmd, pdf) = convert(
        &["--url-allow-private", "--quiet", "--links", "reading.txt"],
        tmp.path(),
    );
    run(cmd);
    let text = chunk_text(&extract(&pdf));
    assert!(text.contains("Remember the milk") && text.contains("The heron returned"));
}
