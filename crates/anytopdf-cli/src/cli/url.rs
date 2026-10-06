use crate::fetch::{SnapshotMode, UrlMode, UrlOptions};
use std::path::PathBuf;

/// How http(s) inputs are fetched. Each option can also be set with its ANYTOPDF_URL_* variable.
#[derive(Debug, clap::Args)]
#[command(next_help_heading = "URL inputs")]
pub(crate) struct UrlArgs {
    /// Convert every link in FILE (repeatable): one URL per line, a browser bookmark
    /// export (HTML) or Chrome's Bookmarks JSON. Bookmark folders become nested PDF
    /// bookmarks; an unreachable link is skipped with a warning.
    #[arg(long, value_name = "FILE")]
    pub(crate) links: Vec<PathBuf>,

    /// How to fetch URLs: auto uses yt-dlp for known video and podcast hosts and
    /// downloads anything else; page always downloads; media always uses yt-dlp.
    #[arg(long, env = "ANYTOPDF_URL_MODE", value_enum, default_value = "auto")]
    pub(crate) url_mode: UrlMode,

    /// Add a rendered snapshot of web pages printed by headless Chrome, Chromium or
    /// Edge (auto: only when one is installed together with Poppler pdftoppm).
    #[arg(
        long,
        env = "ANYTOPDF_URL_SNAPSHOT",
        value_enum,
        default_value = "auto"
    )]
    pub(crate) url_snapshot: SnapshotMode,

    /// Caption languages yt-dlp downloads, in its --sub-langs syntax.
    #[arg(long, env = "ANYTOPDF_URL_SUB_LANGS", default_value = "en.*,en")]
    pub(crate) url_sub_langs: String,

    /// Highest video resolution yt-dlp downloads.
    #[arg(long, env = "ANYTOPDF_URL_MAX_HEIGHT", default_value_t = 720)]
    pub(crate) url_max_height: u32,

    /// Largest download per URL, in MiB.
    #[arg(long, env = "ANYTOPDF_URL_MAX_MB", default_value_t = 1024)]
    pub(crate) url_max_mb: u64,

    /// Seconds allowed for each URL download, snapshot or yt-dlp run.
    #[arg(long, env = "ANYTOPDF_URL_TIMEOUT", default_value_t = 600)]
    pub(crate) url_timeout: u64,

    /// Allow URLs on loopback, private and link-local addresses (refused by default).
    #[arg(long, env = "ANYTOPDF_URL_ALLOW_PRIVATE", value_parser = clap::builder::BoolishValueParser::new())]
    pub(crate) url_allow_private: bool,
}

impl UrlArgs {
    pub(crate) fn options(&self) -> UrlOptions {
        UrlOptions {
            mode: self.url_mode,
            snapshot: self.url_snapshot,
            sub_langs: self.url_sub_langs.clone(),
            max_height: self.url_max_height,
            max_mb: self.url_max_mb,
            timeout: std::time::Duration::from_secs(self.url_timeout.max(1)),
            allow_private: self.url_allow_private,
        }
    }
}
