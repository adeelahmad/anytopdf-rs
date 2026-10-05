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
    /// List the jobs and webhook deliveries in a queue directory.
    Status {
        /// Queue directory.
        queue: PathBuf,
    },
    /// Print a new webhook signing secret for ANYTOPDF_WEBHOOK_SECRET.
    Secret,
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
