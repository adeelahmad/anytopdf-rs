use clap::Subcommand;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub(crate) enum SetupCommand {
    /// Turn on speech-to-text: install a checksummed whisper.cpp model into the
    /// user data folder and record it for the bundled Whisper plugin.
    #[command(after_long_help = SETUP_WHISPER_HELP)]
    Whisper(SetupWhisperArgs),
}

const SETUP_WHISPER_HELP: &str = "\
The model goes to <data>/whisper/ggml-<model>.bin, where <data> is ANYTOPDF_DATA_DIR
or the per-user data folder (~/Library/Application Support/anytopdf on macOS,
%LOCALAPPDATA%\\anytopdf on Windows, ~/.local/share/anytopdf elsewhere). Every
download is checked against the SHA-1 whisper.cpp publishes for that model.

Transcription also needs a Whisper engine (whisper.cpp's whisper-cli, or
whisper-ctranslate2 / openai-whisper) and FFmpeg on PATH. Once all three are
present, the Whisper plugin shipped beside anytopdf turns on by itself; this
command and `anytopdf doctor` say what is still missing.";

#[derive(Debug, clap::Args)]
pub(crate) struct SetupWhisperArgs {
    /// Model to install; `.en` models are English-only, `-q5_0` ones are quantized.
    #[arg(long, default_value = anytopdf_plugin_whisper::setup::DEFAULT_MODEL,
          value_parser = clap::builder::PossibleValuesParser::new(
              anytopdf_plugin_whisper::setup::MODELS.iter().map(|m| m.name)))]
    pub(crate) model: String,
    /// Install from a ggml file already on disk instead of downloading it.
    #[arg(long, value_name = "FILE")]
    pub(crate) from: Option<PathBuf>,
    /// Download from this mirror of the whisper.cpp model folder.
    #[arg(long, value_name = "URL", env = "ANYTOPDF_WHISPER_MODEL_URL",
          default_value = anytopdf_plugin_whisper::setup::DEFAULT_BASE_URL)]
    pub(crate) base_url: String,
    /// Download again even when a verified copy is already installed.
    #[arg(long)]
    pub(crate) force: bool,
}
