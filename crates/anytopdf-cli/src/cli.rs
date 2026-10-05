use anytopdf_builtin::OcrMode;
use anytopdf_core::Profile;
use clap::{Parser, Subcommand, builder::TypedValueParser};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "anytopdf",
    version,
    about = "Media and text to searchable PDF",
    arg_required_else_help = true
)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
    /// Disable discovery and execution of runtime plugins.
    #[arg(long, global = true)]
    pub(crate) no_plugins: bool,
    /// Maximum duration of each runtime plugin invocation, in seconds.
    #[arg(long, global = true, default_value = "60", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) plugin_timeout: u64,
    /// Register only these plugin capability kinds (repeatable).
    #[arg(long, global = true, value_parser = ["importer", "source-enricher", "graph-enricher", "unit-enricher", "renderer"])]
    pub(crate) allow_plugin_kind: Vec<String>,
    /// Deny these plugin capability kinds; deny takes precedence.
    #[arg(long, global = true, value_parser = ["importer", "source-enricher", "graph-enricher", "unit-enricher", "renderer"])]
    pub(crate) deny_plugin_kind: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Commands {
    /// Convert media and documents into one searchable PDF.
    Convert(Box<ConvertArgs>),
    /// Report which optional runtime providers are available.
    Doctor {
        /// Emit one JSON document on stdout.
        #[arg(long)]
        json: bool,
    },
    /// List discovered runtime plugins and their capabilities.
    Plugins {
        /// Emit one JSON document on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Extract text and provenance from a PDF produced by anytopdf.
    Extract {
        /// PDF file to read.
        pdf: PathBuf,
        /// Emit one JSON document on stdout (extract always does).
        #[arg(long)]
        json: bool,
    },
    /// Show what this binary and environment support, and how to enable or add more.
    #[command(after_long_help = crate::environment::HELP_FOOTER)]
    Capabilities {
        /// Emit one JSON document on stdout: exit codes, diagnostic codes, profiles,
        /// OCR modes, importers and schema ids.
        #[arg(long)]
        json: bool,
    },
    /// Watch a mail source and convert each new message into its own PDF.
    #[cfg_attr(not(feature = "imap"), command(hide = true))]
    Watch {
        #[command(subcommand)]
        source: WatchSource,
    },
    /// Detect the format and importer for an input without converting it.
    Probe {
        /// File to inspect.
        input: PathBuf,
        /// Emit one JSON document on stdout (probe always does).
        #[arg(long)]
        json: bool,
    },
    /// Serve convert, extract, probe and capabilities as MCP tools over stdio.
    Mcp,
}

#[derive(Debug, clap::Args)]
pub(crate) struct ConvertArgs {
    /// Files or directories to convert.
    #[arg(required = true)]
    pub(crate) inputs: Vec<PathBuf>,

    /// Replace an existing output; source files are always protected.
    #[arg(long)]
    pub(crate) overwrite: bool,

    /// Fail before publishing output when ingestion or rendering reports warnings;
    /// informational notices are ignored.
    #[arg(long)]
    pub(crate) strict: bool,

    /// Abort without publishing when any input fails (default: skip failing inputs and continue).
    #[arg(long)]
    pub(crate) fail_fast: bool,

    /// Renderer plugin that writes the output.
    #[arg(long, default_value = "pdf")]
    pub(crate) renderer: String,

    /// Output PDF path.
    #[arg(short, long)]
    pub(crate) output: Option<PathBuf>,

    /// Write one PDF per input into this directory instead of one merged PDF.
    #[arg(long, conflicts_with = "output")]
    pub(crate) output_dir: Option<PathBuf>,

    /// Only convert discovered inputs whose path matches this regular expression.
    #[arg(long)]
    pub(crate) filter: Option<String>,

    /// Include hidden files and directories during discovery.
    #[arg(long)]
    pub(crate) include_hidden: bool,

    /// OCR provider selection.
    #[arg(long, default_value = "auto", value_parser = clap::builder::PossibleValuesParser::new([
        clap::builder::PossibleValue::new("auto"),
        clap::builder::PossibleValue::new("vision"),
        clap::builder::PossibleValue::new("doctr"),
        clap::builder::PossibleValue::new("tesseract"),
        clap::builder::PossibleValue::new("off").alias("none"),
    ]).try_map(|s| s.parse::<OcrMode>()))]
    pub(crate) ocr: OcrMode,

    /// OCR language code.
    #[arg(long, default_value = "eng")]
    pub(crate) lang: String,

    /// Transcript file to attach to media (repeatable).
    #[arg(long = "transcript")]
    pub(crate) transcripts: Vec<PathBuf>,

    /// Seconds between sampled video frames.
    #[arg(long, default_value_t = 5.0)]
    pub(crate) video_interval: f64,

    /// Scene-change sensitivity for video frame selection.
    #[arg(long, default_value_t = 0.30)]
    pub(crate) scene_threshold: f64,

    /// Perceptual-hash distance below which video frames count as duplicates.
    #[arg(long, default_value_t = 4)]
    pub(crate) dedupe_distance: u32,

    /// Maximum video frames to keep (0 means unlimited).
    #[arg(long, default_value_t = 0)]
    pub(crate) max_video_frames: usize,

    /// Maximum frames to import from a multi-frame TIFF or GIF (0 means unlimited).
    #[arg(long, default_value_t = 0)]
    pub(crate) max_image_frames: usize,

    /// Ignore subtitle tracks embedded in video files.
    #[arg(long)]
    pub(crate) no_embedded_subtitles: bool,

    /// Write the normalized document graph as JSON to this path.
    #[arg(long)]
    pub(crate) dump_graph: Option<PathBuf>,

    /// Output profile: archive keeps provenance detail, share strips local paths.
    #[arg(long, default_value = "archive")]
    pub(crate) profile: Profile,

    /// Omit the provenance page from the PDF.
    #[arg(long)]
    pub(crate) no_provenance_page: bool,

    /// Suppress progress and diagnostic output on stderr.
    #[arg(short, long)]
    pub(crate) quiet: bool,

    /// Write NDJSON progress events (schema anytopdf.events/1) to stderr instead of human-readable progress and diagnostics.
    #[arg(long)]
    pub(crate) events: bool,

    /// Emit one JSON document on stdout.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum WatchSource {
    /// Fetch new messages from an IMAP mailbox and convert each one.
    ///
    /// Each message is saved as a raw `.eml` file and converted by `anytopdf convert`
    /// into `<output-dir>/<mailbox>-<uidvalidity>-<uid>.pdf`; arguments after `--`
    /// are passed to convert. Turning email into pages needs an importer that accepts
    /// RFC 822 messages (built in or an `anytopdf-plugin-*`). The password is read from
    /// ANYTOPDF_IMAP_PASSWORD or --password-file, never from the command line.
    Imap(Box<ImapArgs>),
}

#[derive(Debug, clap::Args)]
pub(crate) struct ImapArgs {
    /// IMAP server host name.
    #[arg(long, env = "ANYTOPDF_IMAP_HOST")]
    pub(crate) host: String,

    /// Server port (default 993 for implicit TLS, 143 otherwise).
    #[arg(long, env = "ANYTOPDF_IMAP_PORT")]
    pub(crate) port: Option<u16>,

    /// Connection security: implicit TLS, STARTTLS, or none (loopback hosts only).
    #[arg(long, env = "ANYTOPDF_IMAP_TLS", default_value = "implicit", value_parser = ["implicit", "starttls", "none"])]
    pub(crate) tls: String,

    /// Login user name.
    #[arg(long, env = "ANYTOPDF_IMAP_USER")]
    pub(crate) user: String,

    /// Read the password from this file instead of ANYTOPDF_IMAP_PASSWORD.
    #[arg(long, env = "ANYTOPDF_IMAP_PASSWORD_FILE")]
    pub(crate) password_file: Option<PathBuf>,

    /// Mailbox to watch.
    #[arg(long, env = "ANYTOPDF_IMAP_MAILBOX", default_value = "INBOX")]
    pub(crate) mailbox: String,

    /// Extra IMAP SEARCH criteria, e.g. 'FROM "scanner@example.com"'.
    #[arg(long)]
    pub(crate) search: Option<String>,

    /// PEM file of additional trusted CA certificates (for self-signed servers).
    #[arg(long)]
    pub(crate) ca_file: Option<PathBuf>,

    /// Directory that receives one PDF per message.
    #[arg(long)]
    pub(crate) output_dir: PathBuf,

    /// Directory for watcher progress, spooled and failed messages
    /// (default: <output-dir>/.anytopdf-imap). Use one per mailbox.
    #[arg(long)]
    pub(crate) state_dir: Option<PathBuf>,

    /// On first run, also convert messages already in the mailbox.
    #[arg(long)]
    pub(crate) backfill: bool,

    /// Check the mailbox once and exit instead of watching.
    #[arg(long)]
    pub(crate) once: bool,

    /// Seconds between checks (also the IDLE refresh interval).
    #[arg(long, default_value = "60", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) poll_interval: u64,

    /// Skip messages larger than this many bytes.
    #[arg(long, default_value_t = 50 * 1024 * 1024)]
    pub(crate) max_message_bytes: u64,

    /// Conversion attempts before a message is moved to <state-dir>/failed.
    #[arg(long, default_value = "3", value_parser = clap::value_parser!(u32).range(1..))]
    pub(crate) max_attempts: u32,

    /// Maximum seconds for one message conversion.
    #[arg(long, default_value = "600", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) convert_timeout: u64,

    /// Set the \Seen flag on converted messages.
    #[arg(long)]
    pub(crate) mark_seen: bool,

    /// Move converted messages to this mailbox.
    #[arg(long)]
    pub(crate) move_to: Option<String>,

    /// Keep converted messages' .eml files in <state-dir>/spool.
    #[arg(long)]
    pub(crate) keep_eml: bool,

    /// Suppress per-message progress on stderr.
    #[arg(short, long)]
    pub(crate) quiet: bool,

    /// Options passed to each `anytopdf convert` run (after `--`).
    #[arg(last = true)]
    pub(crate) convert_args: Vec<std::ffi::OsString>,
}
