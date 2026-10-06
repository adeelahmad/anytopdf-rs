use clap::Subcommand;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub(crate) enum CaptureCommand {
    /// Record the screen with FFmpeg until --duration or Ctrl-C, then convert it.
    ///
    /// Uses FFmpeg's platform grabber: avfoundation on macOS (needs the Screen
    /// Recording permission), gdigrab or ddagrab on Windows, x11grab on Linux. Pages
    /// are picked like any video: one frame every --interval seconds plus every scene
    /// change, with near-duplicate frames dropped.
    Screen(Box<ScreenArgs>),
}

#[derive(Debug, clap::Args)]
pub(crate) struct ScreenArgs {
    /// Screen to record: the macOS screen index, the Windows output index (ddagrab;
    /// the whole desktop through gdigrab when omitted) or the X11 display number on
    /// Linux ($DISPLAY when omitted).
    #[arg(long)]
    pub(crate) display: Option<u32>,
    /// Seconds between sampled frames; scene changes are kept as well.
    #[arg(long, default_value_t = 5.0)]
    pub(crate) interval: f64,
    /// Stop after this many seconds (default: record until Ctrl-C).
    #[arg(long)]
    pub(crate) duration: Option<f64>,
    /// Frames recorded per second; scene changes shorter than a frame are missed.
    #[arg(long, default_value_t = 2.0)]
    pub(crate) framerate: f64,
    /// FFmpeg input format to use instead of the platform grabber, e.g. kmsgrab.
    #[arg(long, requires = "input")]
    pub(crate) input_format: Option<String>,
    /// FFmpeg input (`-i` value) for --input-format.
    #[arg(long, requires = "input_format")]
    pub(crate) input: Option<String>,
    /// Keep the recording at this path (Matroska video) instead of deleting it.
    #[arg(long)]
    pub(crate) keep_recording: Option<PathBuf>,
    /// Output PDF path (default: screen-<UTC time>.pdf in the current directory).
    #[arg(short, long)]
    pub(crate) output: Option<PathBuf>,
    /// Convert options after `--`, for example `-- --ocr off --scene-threshold 0.2`.
    #[arg(last = true)]
    pub(crate) convert: Vec<String>,
}
