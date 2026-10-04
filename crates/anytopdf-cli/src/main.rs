mod argv;
mod exit;
use anyhow::{Context, Result, bail};
use anytopdf_builtin::{
    BuiltinOptions, DiscoveryOptions, OcrEnricher, OcrMode, detect_providers, discover_inputs,
    register_builtins,
};
use anytopdf_core::{
    Channel, ChunkSet, Diagnostic, DiagnosticCode, Manifest, Pipeline, Profile, Registry,
    RuntimePluginPolicy, Severity, atomic_write, register_runtime_plugins_with_policy,
    strip_workspace_paths,
};
use anytopdf_pdf::{EmbeddedFile, SearchablePdfRenderer, embed_files};
use clap::{Parser, Subcommand};
use exit::{CliError, ExitClass, fail, tag};
use regex::Regex;
use std::{
    path::{Path, PathBuf},
    process::ExitCode,
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
    Doctor {
        /// Emit one JSON document on stdout.
        #[arg(long)]
        json: bool,
    },
    Plugins {
        /// Emit one JSON document on stdout.
        #[arg(long)]
        json: bool,
    },
    Probe {
        input: PathBuf,
        /// Emit one JSON document on stdout (probe always does).
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, clap::Args)]
struct ConvertArgs {
    #[arg(required = true)]
    inputs: Vec<PathBuf>,

    /// Replace an existing output; source files are always protected.
    #[arg(long)]
    overwrite: bool,

    /// Fail before publishing output when ingestion or rendering reports warnings;
    /// informational notices are ignored.
    #[arg(long)]
    strict: bool,

    /// Abort without publishing when any input fails (default: skip failing inputs and continue).
    #[arg(long)]
    fail_fast: bool,

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

    /// Output profile: archive keeps provenance detail, share strips local paths.
    #[arg(long, default_value = "archive")]
    profile: Profile,

    /// Omit the provenance page from the PDF.
    #[arg(long)]
    no_provenance_page: bool,

    #[arg(short, long)]
    quiet: bool,
}

fn main() -> ExitCode {
    let cli = match Cli::try_parse_from(argv::normalize_args(std::env::args_os().collect())) {
        Ok(cli) => cli,
        Err(e) => {
            let class = if e.use_stderr() {
                ExitClass::Usage
            } else {
                ExitClass::Success
            };
            let _ = e.print();
            return ExitCode::from(class.code());
        }
    };
    match run(cli) {
        Ok(()) => ExitCode::from(ExitClass::Success.code()),
        Err(e) => {
            eprintln!("error: {:#}", e.error);
            ExitCode::from(e.class.code())
        }
    }
}

fn run(cli: Cli) -> Result<(), CliError> {
    let policy = RuntimePluginPolicy {
        enabled: !cli.no_plugins,
        timeout: Duration::from_secs(cli.plugin_timeout),
        allow_capabilities: (!cli.allow_plugin_kind.is_empty())
            .then(|| cli.allow_plugin_kind.into_iter().collect()),
        deny_capabilities: cli.deny_plugin_kind.into_iter().collect(),
    };
    match cli.command {
        Commands::Convert(args) => convert(*args, &policy),
        Commands::Doctor { json } => Ok(doctor(json)?),
        Commands::Plugins { json } => Ok(plugins(&policy, json)?),
        Commands::Probe { input, .. } => probe(&input, &policy),
    }
}

fn registry(opts: BuiltinOptions, policy: &RuntimePluginPolicy) -> (Registry, Vec<Diagnostic>) {
    let mut registry = Registry::default();
    register_builtins(&mut registry, opts);
    registry.register_renderer(Arc::new(SearchablePdfRenderer::default()));
    let warnings = register_runtime_plugins_with_policy(&mut registry, policy);
    (registry, warnings)
}

fn convert(args: ConvertArgs, policy: &RuntimePluginPolicy) -> Result<(), CliError> {
    if !args.video_interval.is_finite() || args.video_interval <= 0.0 {
        return Err(fail(
            ExitClass::Usage,
            "--video-interval must be finite and greater than zero",
        ));
    }
    if !args.scene_threshold.is_finite() || !(0.0..=1.0).contains(&args.scene_threshold) {
        return Err(fail(
            ExitClass::Usage,
            "--scene-threshold must be between 0 and 1",
        ));
    }
    if args.dedupe_distance > 64 {
        return Err(fail(
            ExitClass::Usage,
            "--dedupe-distance must be between 0 and 64",
        ));
    }
    for path in &args.transcripts {
        if !path.is_file() {
            return Err(fail(
                ExitClass::Input,
                format!("transcript not found: {}", path.display()),
            ));
        }
    }
    let created = match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(v) => tag(
            ExitClass::Usage,
            v.trim()
                .parse::<i64>()
                .map_err(|_| anyhow::anyhow!("SOURCE_DATE_EPOCH must be an integer: {v:?}")),
        )?,
        Err(_) => std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64),
    };
    let filter = tag(
        ExitClass::Usage,
        args.filter
            .as_deref()
            .map(Regex::new)
            .transpose()
            .context("invalid --filter regex"),
    )?;

    let inputs = tag(
        ExitClass::Input,
        discover_inputs(
            &args.inputs,
            &DiscoveryOptions {
                include_hidden: args.include_hidden,
                filter,
            },
        ),
    )?;

    // Keep command-line order (stable within a directory) so source order is predictable.
    let roots: Vec<PathBuf> = args
        .inputs
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();
    let mut inputs = inputs;
    inputs.sort_by_key(|i| roots.iter().position(|r| i.starts_with(r)));

    if inputs.is_empty() {
        return Err(fail(ExitClass::Input, "no input files matched"));
    }
    let mut protected = inputs.clone();
    protected.extend(
        args.transcripts
            .iter()
            .map(|p| p.canonicalize())
            .collect::<std::io::Result<Vec<_>>>()?,
    );
    let output = tag(
        ExitClass::Usage,
        checked_destination(&args.output, &protected, args.overwrite),
    )?;
    if let Some(path) = &args.dump_graph {
        let graph = tag(
            ExitClass::Usage,
            checked_destination(path, &protected, args.overwrite),
        )?;
        if graph == output {
            return Err(fail(
                ExitClass::Usage,
                "PDF and graph outputs must use different paths",
            ));
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
    let mut run = tag(ExitClass::Input, pipeline.ingest(&inputs, args.quiet))?;
    run.warnings.append(&mut warnings);

    for diagnostic in &run.warnings {
        print_diagnostic(diagnostic);
    }

    let skipped: Vec<&Diagnostic> = run.warnings.iter().filter(|d| d.code.is_skip()).collect();
    if !skipped.is_empty() {
        exit::print_summary(run.graph.sources.len(), &skipped);
        if args.fail_fast {
            return Err(fail(
                ExitClass::FailFast,
                "--fail-fast stopped on a skipped input",
            ));
        }
    }

    if !matches!(args.ocr, OcrMode::Auto | OcrMode::Off)
        && let Some(d) = run.warnings.iter().find(|d| {
            d.code == DiagnosticCode::EnrichmentFailed
                && d.message.contains("no OCR provider succeeded")
        })
    {
        return Err(fail(ExitClass::Provider, &d.message));
    }
    if run.graph.units.is_empty() {
        return Err(fail(ExitClass::Input, "no usable content was imported"));
    }
    if args.strict && run.warnings.iter().any(|d| d.severity >= Severity::Warning) {
        return Err(fail(
            ExitClass::Strict,
            "strict conversion stopped on ingestion warnings",
        ));
    }

    let metadata = &mut run.graph.metadata;
    metadata.insert("anytopdf.profile".into(), args.profile.as_str().into());
    metadata.insert("anytopdf.created".into(), created.to_string());
    if args.no_provenance_page {
        metadata.insert("anytopdf.provenance-page".into(), "off".into());
    }
    for p in detect_providers() {
        if let Some(version) = p.version {
            metadata.insert(format!("provider.{}.version", p.name), version);
        }
    }
    let dump = args.dump_graph.as_ref().map(|_| {
        args.profile.filter(
            &strip_workspace_paths(&run.graph, &run.context.workspace),
            Channel::GraphDump,
        )
    });
    // Rendering still needs workspace images, so keep each unit's visual path.
    let mut document = args.profile.filter(&run.graph, Channel::Document);
    for (out, unit) in document.units.iter_mut().zip(&run.graph.units) {
        out.visual_path = unit.visual_path.clone();
    }
    run.graph = document;

    // Render into staging first, including runtime renderers, before publishing output.
    let staged = run.context.workspace.join("result.pdf");
    let report = tag(
        ExitClass::Render,
        pipeline.render(&run, &args.renderer, &staged),
    )?;
    for warning in &report.warnings {
        print_diagnostic(&Diagnostic::new(DiagnosticCode::RenderWarning, warning));
    }
    if args.strict && !report.warnings.is_empty() {
        return Err(fail(
            ExitClass::Strict,
            "strict conversion stopped on rendering warnings",
        ));
    }
    let bytes = tag(
        ExitClass::Render,
        std::fs::read(&staged).context("renderer did not produce output"),
    )?;
    if !bytes.starts_with(b"%PDF-") || report.pages == 0 {
        return Err(fail(
            ExitClass::Render,
            "renderer did not produce a valid PDF",
        ));
    }
    let manifest = serde_json::to_vec_pretty(&Manifest::build(&run.graph, &report))?;
    let chunks = serde_json::to_vec_pretty(&ChunkSet::build(&run.graph, &report))?;
    let files = [
        EmbeddedFile {
            name: "anytopdf-manifest.json".into(),
            mime_type: "application/json".into(),
            bytes: manifest.clone(),
        },
        EmbeddedFile {
            name: "anytopdf-chunks.json".into(),
            mime_type: "application/json".into(),
            bytes: chunks.clone(),
        },
    ];
    let (bytes, sidecars) = match embed_files(&bytes, &files) {
        Ok(embedded) => (embedded, None),
        Err(e) => {
            print_diagnostic(&Diagnostic::new(
                DiagnosticCode::ManifestSidecar,
                format!("could not embed manifest and chunks ({e}); writing sidecar files"),
            ));
            (bytes, Some((manifest, chunks)))
        }
    };
    publish_output(&args.output, &bytes, args.overwrite)?;
    if let Some((manifest, chunks)) = sidecars {
        for (suffix, data) in [("manifest", manifest), ("chunks", chunks)] {
            let mut name = args.output.as_os_str().to_owned();
            name.push(format!(".{suffix}.json"));
            publish_output(Path::new(&name), &data, args.overwrite)?;
        }
    }
    if let (Some(path), Some(graph)) = (&args.dump_graph, &dump) {
        publish_output(path, &serde_json::to_vec_pretty(graph)?, args.overwrite)?;
    }

    if !args.quiet {
        eprintln!("Wrote {} ({} pages)", args.output.display(), report.pages);
    }
    Ok(())
}

fn print_diagnostic(d: &Diagnostic) {
    let level = match d.severity {
        Severity::Info => "INFO",
        Severity::Warning => "WARNING",
    };
    eprintln!("{level} [{}]: {}", d.code.as_str(), d.message);
}

fn doctor(json: bool) -> Result<()> {
    if json {
        let providers: Vec<_> = detect_providers()
            .into_iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name, "available": p.available,
                    "path": p.path.map(|path| path.display().to_string()),
                    "version": p.version
                })
            })
            .collect();
        let ocr: Vec<_> = OcrEnricher::status()
            .into_iter()
            .map(|s| serde_json::json!({"name": s.name, "available": s.available, "detail": s.detail}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": "anytopdf.doctor/1", "providers": providers, "ocr": ocr
            }))?
        );
        return Ok(());
    }
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

fn plugins(policy: &RuntimePluginPolicy, json: bool) -> Result<()> {
    let (registry, warnings) = registry(BuiltinOptions::default(), policy);
    for warning in warnings {
        eprintln!("WARNING: {warning}");
    }
    let mut descriptors = registry.descriptors();
    descriptors.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": "anytopdf.plugins/1", "plugins": descriptors
            }))?
        );
        return Ok(());
    }

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

fn probe(path: &Path, policy: &RuntimePluginPolicy) -> Result<(), CliError> {
    let path = tag(
        ExitClass::Input,
        path.canonicalize()
            .with_context(|| format!("input not found: {}", path.display())),
    )?;
    if !path.is_file() {
        return Err(fail(ExitClass::Input, "probe requires a file"));
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
            "schema_version": "anytopdf.probe/1",
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
