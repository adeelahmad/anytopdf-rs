use anytopdf_builtin::OcrMode;
use anytopdf_core::{Profile, SandboxMode};
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
    /// Confine runtime plugins: off, contain (end every process a plugin starts)
    /// or strict (contain, write only to the job workspace, no network)
    /// [default: off; contain for `queue`].
    #[arg(
        long,
        global = true,
        value_parser = clap::builder::PossibleValuesParser::new(["off", "contain", "strict"])
            .map(|mode| mode.parse::<SandboxMode>().expect("listed sandbox mode"))
    )]
    pub(crate) plugin_sandbox: Option<SandboxMode>,
    /// Extra file or directory a strict-sandboxed plugin may read (repeatable).
    #[arg(long, global = true, value_name = "PATH")]
    pub(crate) plugin_sandbox_allow_read: Vec<PathBuf>,
}

impl Cli {
    /// The sandbox level runtime plugins run under. Queued jobs come from folders and
    /// HTTP uploads rather than an operator at a terminal, so `queue` defaults to
    /// `contain`; everything else defaults to `off`.
    pub(crate) fn sandbox_mode(&self) -> SandboxMode {
        self.plugin_sandbox.unwrap_or(match self.command {
            Commands::Queue { .. } => SandboxMode::Contain,
            _ => SandboxMode::Off,
        })
    }
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
    /// Run a folder-backed conversion queue with a watched inbox and signed webhooks.
    Queue {
        #[command(subcommand)]
        command: QueueCommand,
    },
    /// Serve convert, extract, probe and capabilities as MCP tools over stdio.
    Mcp,
    /// Reach the print helper from other devices: TLS front, users and discovery.
    #[command(subcommand)]
    Print(PrintCommand),
}

#[derive(Debug, Subcommand)]
pub(crate) enum PrintCommand {
    /// Accept print jobs from remote clients over TLS and pass them to the local print helper.
    Remote(Box<RemoteArgs>),
    /// Add or change a print user; reads the password from the first line of stdin.
    Passwd {
        /// User name clients sign in with.
        user: String,
        /// Users file (JSON, Argon2id hashes); created if missing.
        #[arg(long)]
        users: PathBuf,
        /// Remove the user instead of setting a password.
        #[arg(long)]
        delete: bool,
    },
    /// Print unicast DNS-SD records that let remote clients discover the printer.
    DnsSd {
        /// DNS domain the records go in, e.g. home.example.
        #[arg(long)]
        domain: String,
        /// Host name clients connect to, e.g. printer.home.example.
        #[arg(long)]
        host: String,
        /// Port of the remote front.
        #[arg(long, default_value_t = anytopdf_print::DEFAULT_PORT)]
        port: u16,
        /// Printer name shown to users.
        #[arg(long, default_value = "anytopdf")]
        name: String,
        /// Also emit A/AAAA records for the host (repeatable).
        #[arg(long = "address")]
        addresses: Vec<std::net::IpAddr>,
        /// Emit one JSON document on stdout.
        #[arg(long)]
        json: bool,
    },
    /// Advertise the printer on the local network over multicast DNS until stopped.
    Advertise {
        /// Port of the remote front.
        #[arg(long, default_value_t = anytopdf_print::DEFAULT_PORT)]
        port: u16,
        /// Printer name shown to users.
        #[arg(long, default_value = "anytopdf")]
        name: String,
        /// Multicast DNS host name for this machine.
        #[arg(long, default_value = "anytopdf.local")]
        host: String,
        /// Addresses to announce (repeatable); every interface when omitted.
        #[arg(long = "address")]
        addresses: Vec<std::net::IpAddr>,
    },
    /// Print the ipps:// URL to add the printer by hand (Windows, Android).
    Url {
        /// Host name or address clients connect to.
        #[arg(long)]
        host: String,
        /// Port of the remote front.
        #[arg(long, default_value_t = anytopdf_print::DEFAULT_PORT)]
        port: u16,
    },
}

#[derive(Debug, clap::Args)]
pub(crate) struct RemoteArgs {
    /// Address to accept clients on: the tailnet or WireGuard address, not 0.0.0.0.
    #[arg(long)]
    pub(crate) listen: std::net::SocketAddr,
    /// Loopback address of the print helper.
    #[arg(long, default_value = "127.0.0.1:8631")]
    pub(crate) upstream: std::net::SocketAddr,
    /// PEM certificate chain, e.g. from `tailscale cert`.
    #[arg(long)]
    pub(crate) tls_cert: PathBuf,
    /// PEM private key for --tls-cert.
    #[arg(long)]
    pub(crate) tls_key: PathBuf,
    /// Users file written by `anytopdf print passwd`; required off loopback.
    #[arg(long)]
    pub(crate) users: Option<PathBuf>,
    /// Admit peers in this CIDR (repeatable); required off loopback.
    #[arg(long)]
    pub(crate) allow: Vec<String>,
    /// Admit only Tailscale peers (100.64.0.0/10 and fd7a:115c:a1e0::/48).
    #[arg(long)]
    pub(crate) allow_tailnet: bool,
    /// Append one JSON line per submitted job (time, peer address, user, job name) to this file.
    #[arg(long)]
    pub(crate) receipts: Option<PathBuf>,
    /// Accept listening on every interface or admitting every peer.
    #[arg(long)]
    pub(crate) allow_public_bind: bool,
    /// Maximum simultaneous client connections.
    #[arg(long, default_value_t = 16, value_parser = clap::value_parser!(u16).range(1..))]
    pub(crate) max_connections: u16,
    /// Close a connection after this many idle seconds.
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) idle_timeout: u64,
}

#[derive(Debug, Subcommand)]
pub(crate) enum QueueCommand {
    /// Add a conversion job to a queue directory and print its id.
    Add {
        /// Queue directory (created if missing).
        queue: PathBuf,
        /// Files or directories to convert.
        #[arg(required = true)]
        inputs: Vec<PathBuf>,
        /// Convert options after `--`, for example `-- --ocr off --profile share`.
        #[arg(last = true)]
        convert: Vec<String>,
    },
    /// Convert queued jobs and files dropped into the inbox, delivering webhooks.
    Work(QueueWorkArgs),
    /// Accept authenticated HTTP uploads as jobs (loopback unless TLS is set).
    Serve(QueueServeArgs),
    /// List the jobs and webhook deliveries in a queue directory.
    Status {
        /// Queue directory.
        queue: PathBuf,
    },
    /// Print a new webhook signing secret for ANYTOPDF_WEBHOOK_SECRET.
    Secret,
}

#[derive(Debug, clap::Args)]
pub(crate) struct QueueServeArgs {
    /// Queue directory (created if missing).
    pub(crate) queue: PathBuf,
    /// Address to listen on; anything but loopback needs --tls-cert and --tls-key.
    #[arg(long, default_value = "127.0.0.1:8640")]
    pub(crate) listen: std::net::SocketAddr,
    /// PEM certificate chain for HTTPS.
    #[arg(long, requires = "tls_key")]
    pub(crate) tls_cert: Option<PathBuf>,
    /// PEM private key for --tls-cert.
    #[arg(long, requires = "tls_cert")]
    pub(crate) tls_key: Option<PathBuf>,
    /// Accept listening on every interface (0.0.0.0 or ::).
    #[arg(long)]
    pub(crate) allow_public_bind: bool,
    /// Largest accepted upload, in MiB.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u64).range(1..=65_536))]
    pub(crate) max_upload_mb: u64,
    /// Maximum simultaneous client connections.
    #[arg(long, default_value_t = 8, value_parser = clap::value_parser!(u16).range(1..))]
    pub(crate) max_connections: u16,
    /// Suppress the server log on stderr.
    #[arg(short, long)]
    pub(crate) quiet: bool,
    /// Convert options for uploaded files after `--`, for example `-- --profile share`.
    #[arg(last = true)]
    pub(crate) convert: Vec<String>,
}

#[derive(Debug, clap::Args)]
pub(crate) struct QueueWorkArgs {
    /// Queue directory (created if missing).
    pub(crate) queue: PathBuf,
    /// Exit once the queue and inbox are empty instead of watching.
    #[arg(long)]
    pub(crate) once: bool,
    /// Seconds between inbox scans; a file is claimed once two scans agree on it.
    #[arg(long, default_value_t = 2.0)]
    pub(crate) poll_interval: f64,
    /// Seconds before a running conversion is stopped and its job fails.
    #[arg(long, default_value = "3600", value_parser = clap::value_parser!(u64).range(1..))]
    pub(crate) job_timeout: u64,
    /// POST signed job.received, job.completed and job.failed events here (repeatable);
    /// requires ANYTOPDF_WEBHOOK_SECRET.
    #[arg(long)]
    pub(crate) webhook: Vec<String>,
    /// Suppress the worker log on stderr.
    #[arg(short, long)]
    pub(crate) quiet: bool,
    /// Convert options for inbox files after `--`, for example `-- --ocr off`.
    #[arg(last = true)]
    pub(crate) convert: Vec<String>,
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

    /// Renderer plugin that writes the output: `pdfa` (tagged PDF/A-3a) or `pdf` (printpdf).
    #[arg(long, default_value = "pdfa")]
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

    /// Draw labelled boxes for detected regions over image and video-frame pages.
    ///
    /// KINDS is a comma-separated list of objects, faces and ocr, or all; a bare
    /// --draw-boxes draws objects and faces. Face boxes show the matched person's name
    /// when a recognizer supplied one. The source images are never modified.
    #[arg(
        long,
        value_name = "KINDS",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "objects,faces",
        value_parser = parse_draw_boxes
    )]
    pub(crate) draw_boxes: Option<String>,

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
    /// into `<output-dir>/<mailbox>-<uidvalidity>-<uid>.pdf`, or queued as a job with
    /// --queue; arguments after `--` are passed to convert. Secrets come from
    /// ANYTOPDF_IMAP_PASSWORD, ANYTOPDF_IMAP_OAUTH_TOKEN or a file, never from the
    /// command line.
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

    /// Login method: IMAP LOGIN with a password, or SASL XOAUTH2 (Gmail,
    /// Microsoft 365) with an access token.
    #[arg(long, env = "ANYTOPDF_IMAP_AUTH", default_value = "login", value_parser = ["login", "xoauth2"])]
    pub(crate) auth: String,

    /// With --auth xoauth2: read the access token from this file on every connection
    /// instead of ANYTOPDF_IMAP_OAUTH_TOKEN, so a token refresher can update it.
    #[arg(long, env = "ANYTOPDF_IMAP_OAUTH_TOKEN_FILE")]
    pub(crate) oauth_token_file: Option<PathBuf>,

    /// Only convert mail from this address or domain (repeatable), e.g.
    /// scanner@example.com or example.com. Other mail is skipped and left in place.
    #[arg(long)]
    pub(crate) allow_from: Vec<String>,

    /// Also require dmarc=pass in the Authentication-Results header added by this
    /// receiving server, e.g. mx.google.com or outlook.com.
    #[arg(long, value_name = "AUTHSERV_ID")]
    pub(crate) require_dmarc: Option<String>,

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
    #[arg(long, required_unless_present = "queue")]
    pub(crate) output_dir: Option<PathBuf>,

    /// Hand each message to this `anytopdf queue` directory instead of converting it
    /// here; run `anytopdf queue work` on it to convert.
    #[arg(long, conflicts_with = "output_dir")]
    pub(crate) queue: Option<PathBuf>,

    /// Directory for watcher progress, spooled and failed messages
    /// (default: <output-dir or queue>/.anytopdf-imap). Use one per mailbox.
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

fn parse_draw_boxes(value: &str) -> Result<String, String> {
    anytopdf_pdf::parse_box_kinds(value).map(|kinds| anytopdf_pdf::box_kinds_value(&kinds))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn mode(args: &[&str]) -> SandboxMode {
        Cli::try_parse_from(std::iter::once("anytopdf").chain(args.iter().copied()))
            .unwrap()
            .sandbox_mode()
    }

    #[test]
    fn queue_defaults_to_contain_and_other_commands_to_off() {
        assert_eq!(mode(&["queue", "work", "q"]), SandboxMode::Contain);
        assert_eq!(mode(&["queue", "status", "q"]), SandboxMode::Contain);
        assert_eq!(mode(&["convert", "a.txt"]), SandboxMode::Off);
        assert_eq!(mode(&["plugins"]), SandboxMode::Off);
    }

    #[test]
    fn an_explicit_sandbox_level_overrides_the_queue_default() {
        assert_eq!(
            mode(&["--plugin-sandbox", "off", "queue", "work", "q"]),
            SandboxMode::Off
        );
        assert_eq!(
            mode(&["queue", "work", "q", "--plugin-sandbox", "strict"]),
            SandboxMode::Strict
        );
        assert_eq!(
            mode(&["--plugin-sandbox", "contain", "convert", "a.txt"]),
            SandboxMode::Contain
        );
    }
}
