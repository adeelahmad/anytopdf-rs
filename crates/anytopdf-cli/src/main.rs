use anyhow::{Context, Result, bail};
use anytopdf_builtin::{
    BuiltinOptions, DiscoveryOptions, OcrEnricher, OcrMode, discover_inputs, register_builtins,
};
use anytopdf_core::{
    Pipeline, Registry, RuntimePluginPolicy, atomic_write, register_runtime_plugins_with_policy,
};
use anytopdf_pdf::SearchablePdfRenderer;
use clap::{Parser, Subcommand};
use regex::Regex;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

#[derive(Debug, Parser)]
#[command(
    name = "anytopdf",
    version,
    about = "Media and text to searchable PDF",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    /// Disable discovery and execution of runtime plugins.
    #[arg(long, global = true)]
    no_plugins: bool,
    /// Maximum duration of each runtime plugin invocation, in seconds.
    #[arg(long, global = true, default_value = "60", value_parser = clap::value_parser!(u64).range(1..))]
    plugin_timeout: u64,
    /// Register only these plugin capability kinds (repeatable).
    #[arg(long, global = true, value_parser = ["importer", "source-enricher", "graph-enricher", "unit-enricher", "renderer"])]
    allow_plugin_kind: Vec<String>,
    /// Deny these plugin capability kinds; deny takes precedence.
    #[arg(long, global = true, value_parser = ["importer", "source-enricher", "graph-enricher", "unit-enricher", "renderer"])]
    deny_plugin_kind: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Convert(Box<ConvertArgs>),
    Doctor,
    Plugins,
    Probe { input: PathBuf },
}

#[derive(Debug, clap::Args)]
struct ConvertArgs {
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Replace an existing output; source files are always protected.
    #[arg(long)]
    overwrite: bool,

    /// Fail before publishing output when ingestion or rendering reports warnings.
    #[arg(long)]
    strict: bool,

    #[arg(long, default_value = "pdf")]
    renderer: String,

    #[arg(short, long, default_value = "anytopdf.pdf")]
    output: PathBuf,

    #[arg(long)]
    filter: Option<String>,

    #[arg(long)]
    include_hidden: bool,

    #[arg(long, default_value = "auto")]
    ocr: OcrMode,

    #[arg(long, default_value = "eng")]
    lang: String,

    #[arg(long = "transcript")]
    transcripts: Vec<PathBuf>,

    #[arg(long, default_value_t = 5.0)]
    video_interval: f64,

    #[arg(long, default_value_t = 0.30)]
    scene_threshold: f64,

    #[arg(long, default_value_t = 4)]
    dedupe_distance: u32,

    #[arg(long, default_value_t = 0)]
    max_video_frames: usize,

    #[arg(long)]
    no_embedded_subtitles: bool,

    #[arg(long)]
    dump_graph: Option<PathBuf>,

    #[arg(short, long)]
    quiet: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let policy = RuntimePluginPolicy {
        enabled: !cli.no_plugins,
        timeout: Duration::from_secs(cli.plugin_timeout),
        allow_capabilities: (!cli.allow_plugin_kind.is_empty())
            .then(|| cli.allow_plugin_kind.into_iter().collect()),
        deny_capabilities: cli.deny_plugin_kind.into_iter().collect(),
    };
    match cli.command {
        Commands::Convert(args) => convert(*args, &policy),
        Commands::Doctor => doctor(),
        Commands::Plugins => plugins(&policy),
        Commands::Probe { input } => probe(&input, &policy),
    }
}

fn registry(opts: BuiltinOptions, policy: &RuntimePluginPolicy) -> (Registry, Vec<String>) {
    let mut registry = Registry::default();
    register_builtins(&mut registry, opts);
    registry.register_renderer(Arc::new(SearchablePdfRenderer::default()));
    let warnings = register_runtime_plugins_with_policy(&mut registry, policy);
    (registry, warnings)
}

fn convert(args: ConvertArgs, policy: &RuntimePluginPolicy) -> Result<()> {
    if !args.video_interval.is_finite() || args.video_interval <= 0.0 {
        bail!("--video-interval must be finite and greater than zero");
    }
    if !args.scene_threshold.is_finite() || !(0.0..=1.0).contains(&args.scene_threshold) {
        bail!("--scene-threshold must be between 0 and 1");
    }
    if args.dedupe_distance > 64 {
        bail!("--dedupe-distance must be between 0 and 64");
    }
    for path in &args.transcripts {
        if !path.is_file() {
            bail!("transcript not found: {}", path.display());
        }
    }
    let filter = args
        .filter
        .as_deref()
        .map(Regex::new)
        .transpose()
        .context("invalid --filter regex")?;

    let inputs = discover_inputs(
        &args.inputs,
        &DiscoveryOptions {
            include_hidden: args.include_hidden,
            filter,
        },
    )?;

    if inputs.is_empty() {
        bail!("no input files matched");
    }
    let mut protected = inputs.clone();
    protected.extend(
        args.transcripts
            .iter()
            .map(|p| p.canonicalize())
            .collect::<std::io::Result<Vec<_>>>()?,
    );
    let output = checked_destination(&args.output, &protected, args.overwrite)?;
    if let Some(path) = &args.dump_graph {
        if checked_destination(path, &protected, args.overwrite)? == output {
            bail!("PDF and graph outputs must use different paths");
        }
    }

    let opts = BuiltinOptions {
        video_interval: args.video_interval,
        scene_threshold: args.scene_threshold,
        dedupe_distance: args.dedupe_distance,
        max_video_frames: args.max_video_frames,
        ocr: args.ocr,
        ocr_language: args.lang,
        explicit_transcripts: args.transcripts,
        embedded_subtitles: !args.no_embedded_subtitles,
    };

    let (registry, mut warnings) = registry(opts, policy);
    let pipeline = Pipeline::new(registry);
    let mut run = pipeline.ingest(&inputs, args.quiet)?;
    run.warnings.append(&mut warnings);

    for warning in &run.warnings {
        eprintln!("WARNING: {warning}");
    }

    if run.graph.units.is_empty() {
        bail!("no usable content was imported");
    }
    if args.strict && !run.warnings.is_empty() {
        bail!("strict conversion stopped on ingestion warnings");
    }

    // Render into staging first, including runtime renderers, before publishing output.
    let staged = run.context.workspace.join("result.pdf");
    let report = pipeline.render(&run, &args.renderer, &staged)?;
    for warning in &report.warnings {
        eprintln!("WARNING: {warning}");
    }
    if args.strict && !report.warnings.is_empty() {
        bail!("strict conversion stopped on rendering warnings");
    }
    let bytes = std::fs::read(&staged).context("renderer did not produce output")?;
    if !bytes.starts_with(b"%PDF-") || report.pages == 0 {
        bail!("renderer did not produce a valid PDF");
    }
    publish_output(&args.output, &bytes, args.overwrite)?;
    if let Some(path) = &args.dump_graph {
        publish_output(
            path,
            &serde_json::to_vec_pretty(&run.graph)?,
            args.overwrite,
        )?;
    }

    if !args.quiet {
        eprintln!("Wrote {} ({} pages)", args.output.display(), report.pages);
    }
    Ok(())
}

fn doctor() -> Result<()> {
    println!("anytopdf provider diagnostics");
    println!();

    for command in ["ffmpeg", "ffprobe", "exiftool", "tesseract", "python3"] {
        match which::which(command) {
            Ok(path) => println!("[ok]   {command:<10} {}", path.display()),
            Err(_) => println!("[miss] {command:<10}"),
        }
    }

    println!();
    println!("OCR:");
    for status in OcrEnricher::status() {
        println!(
            "{} {:<10} {}",
            if status.available { "[ok]  " } else { "[miss]" },
            status.name,
            status.detail
        );
    }
    Ok(())
}

fn plugins(policy: &RuntimePluginPolicy) -> Result<()> {
    let (registry, warnings) = registry(BuiltinOptions::default(), policy);
    for warning in warnings {
        eprintln!("WARNING: {warning}");
    }
    let mut descriptors = registry.descriptors();
    descriptors.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));

    for p in descriptors {
        println!(
            "{:<17} {:<24} priority={:<4} ext={}",
            p.kind,
            p.name,
            p.priority,
            if p.extensions.is_empty() {
                "-".into()
            } else {
                p.extensions.join(",")
            }
        );
    }
    Ok(())
}

fn probe(path: &Path, policy: &RuntimePluginPolicy) -> Result<()> {
    let path = path
        .canonicalize()
        .with_context(|| format!("input not found: {}", path.display()))?;
    if !path.is_file() {
        bail!("probe requires a file");
    }
    let (registry, warnings) = registry(BuiltinOptions::default(), policy);
    for warning in warnings {
        eprintln!("WARNING: {warning}");
    }
    let mut source = anytopdf_core::SourceRecord::new(path);
    if let Some(kind) = infer::get_from_path(&source.path)? {
        source.detected_type = Some(kind.mime_type().to_string());
    }
    let importer = registry.importer_for(&source)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "source": source, "importer": importer.descriptor()
        }))?
    );
    Ok(())
}

fn checked_destination(path: &Path, sources: &[PathBuf], overwrite: bool) -> Result<PathBuf> {
    let absolute = if path.exists() {
        path.canonicalize()?
    } else {
        // Resolve existing ancestors to catch symlink aliases even for new files.
        let absolute = std::path::absolute(path)?;
        let mut ancestor = absolute.as_path();
        let mut missing = Vec::new();
        while !ancestor.exists() {
            missing.push(
                ancestor
                    .file_name()
                    .context("invalid output path")?
                    .to_os_string(),
            );
            ancestor = ancestor.parent().context("invalid output parent")?;
        }
        let mut resolved = ancestor.canonicalize()?;
        for part in missing.into_iter().rev() {
            resolved.push(part);
        }
        resolved
    };
    if sources.contains(&absolute) {
        bail!("output would overwrite an input: {}", path.display());
    }
    if path.is_dir() {
        bail!("output is a directory: {}", path.display());
    }
    if path.exists() && !overwrite {
        bail!(
            "output exists: {}; use --overwrite to replace it",
            path.display()
        );
    }
    Ok(absolute)
}

fn publish_output(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    if overwrite {
        return atomic_write(path, bytes);
    }
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut staged = tempfile::NamedTempFile::new_in(parent)?;
    staged.write_all(bytes)?;
    staged.as_file().sync_all()?;
    staged
        .persist_noclobber(path)
        .with_context(|| format!("publish {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_is_protected_even_with_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("source.txt");
        std::fs::write(&input, "keep").unwrap();
        assert!(checked_destination(&input, &[input.canonicalize().unwrap()], true).is_err());
    }

    #[test]
    fn no_arguments_shows_help_without_converting_current_directory() {
        assert_eq!(
            Cli::try_parse_from(["anytopdf"]).unwrap_err().kind(),
            clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    #[test]
    fn publication_does_not_clobber_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.pdf");
        std::fs::write(&path, "keep").unwrap();
        assert!(publish_output(&path, b"replace", false).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "keep");
    }
}
