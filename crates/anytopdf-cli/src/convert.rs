mod stage;

use crate::cli::ConvertArgs;
use crate::events::{Event, EventWriter, RunStatus};
use crate::exit::{CliError, ExitClass, Redactor, fail, tag};
use crate::naming;
use crate::publish::{checked_destination, publish_output};
use anyhow::{Context, Result};
use anytopdf_builtin::{
    BuiltinOptions, ChatOptions, DiscoveryOptions, OcrMode, PROVIDER_NAMES, URL_PROVIDERS,
    detect_providers_named, discover_inputs, register_builtins,
};
use anytopdf_core::{
    Channel, Diagnostic, DocumentGraph, Pipeline, PipelineEvent, PipelineObserver, Profile,
    Registry, RuntimePluginPolicy, Severity, Stage, register_runtime_plugins_with_policy,
    strip_workspace_paths,
};
use anytopdf_pdf::{PdfARenderer, SearchablePdfRenderer};
use regex::Regex;
use stage::stage_document;
use std::{
    io::Stderr,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(crate) fn registry(
    opts: BuiltinOptions,
    policy: &RuntimePluginPolicy,
) -> (Registry, Vec<Diagnostic>) {
    let mut registry = Registry::default();
    register_builtins(&mut registry, opts);
    registry.register_renderer(Arc::new(SearchablePdfRenderer::default()));
    registry.register_renderer(Arc::new(PdfARenderer::default()));
    let warnings = register_runtime_plugins_with_policy(&mut registry, policy);
    (registry, warnings)
}

type Sink = Option<EventWriter<Stderr>>;

fn shown(profile: Profile, path: &Path) -> String {
    if profile == Profile::Share {
        anytopdf_core::basename(path)
    } else {
        path.display().to_string()
    }
}

fn emit(sink: &mut Sink, event: Event) {
    if let Some(writer) = sink
        && !writer.is_broken()
    {
        writer.emit(&event);
    }
}

struct EventObserver<'a> {
    sink: &'a mut Sink,
    profile: Profile,
}

impl PipelineObserver for EventObserver<'_> {
    fn on_event(&mut self, event: &PipelineEvent) {
        let profile = self.profile;
        let mapped = match event {
            PipelineEvent::StageStarted(stage) => Event::StageStarted {
                stage: Stage::as_str(*stage),
            },
            PipelineEvent::StageFinished(stage) => Event::StageFinished {
                stage: Stage::as_str(*stage),
            },
            PipelineEvent::SourceStarted { index, path } => Event::SourceStarted {
                index: *index,
                input: shown(profile, path),
            },
            PipelineEvent::SourceImported { index, path, units } => Event::SourceImported {
                index: *index,
                input: shown(profile, path),
                units: *units,
            },
            PipelineEvent::SourceSkipped { index, path, code } => Event::SourceSkipped {
                index: *index,
                input: shown(profile, path),
                code: code.as_str().to_string(),
            },
            PipelineEvent::UnitStarted {
                unit,
                source,
                enricher,
            } => Event::UnitStarted {
                unit: *unit,
                source: *source,
                enricher: enricher.clone(),
            },
            PipelineEvent::UnitFinished {
                unit,
                source,
                enricher,
            } => Event::UnitFinished {
                unit: *unit,
                source: *source,
                enricher: enricher.clone(),
            },
        };
        emit(self.sink, mapped);
    }
}

fn report_diagnostic(sink: &mut Sink, d: &Diagnostic, redactor: &Redactor, profile: Profile) {
    if sink.is_none() {
        print_redacted(d, redactor);
        return;
    }
    emit(
        sink,
        Event::Diagnostic {
            code: d.code.as_str().to_string(),
            severity: if d.severity == Severity::Info {
                "info"
            } else {
                "warning"
            },
            message: redactor.apply(&d.message),
            input: d.input.as_deref().map(|p| shown(profile, p)),
        },
    );
}

pub(crate) fn convert(args: ConvertArgs, policy: &RuntimePluginPolicy) -> Result<(), CliError> {
    let json = args.json;
    let profile = args.profile;
    let mut sink: Sink = args.events.then(|| EventWriter::new(std::io::stderr()));
    emit(
        &mut sink,
        Event::RunStarted {
            profile: profile.as_str(),
            inputs: args.inputs.len(),
        },
    );
    let mut outcome = ConvertOutcome::default();
    let mut redactor = Redactor::new(profile == Profile::Share);
    for path in args
        .inputs
        .iter()
        .filter(|p| !crate::fetch::is_url(p))
        .chain(&args.transcripts)
        .chain(&args.url.links)
        .chain(args.output.iter())
        .chain(args.output_dir.iter())
        .chain(args.dump_graph.iter())
    {
        redactor.add_parent_of(path);
    }
    let result =
        convert_inner(args, policy, &mut outcome, &mut redactor, &mut sink).map_err(|e| CliError {
            class: e.class,
            error: anyhow::anyhow!("{}", redactor.apply(&format!("{:#}", e.error))),
        });
    let exit_code = result.as_ref().map_or_else(|e| e.class.code(), |_| 0);
    let status = match (&result, outcome.skipped.is_empty()) {
        (Err(_), _) => RunStatus::Failed,
        (Ok(()), true) => RunStatus::Ok,
        (Ok(()), false) => RunStatus::Partial,
    };
    emit(
        &mut sink,
        Event::RunFinished {
            status: match status {
                RunStatus::Failed => RunStatus::Failed,
                RunStatus::Partial => RunStatus::Partial,
                RunStatus::Ok => RunStatus::Ok,
            },
            exit_code,
            error: result.as_ref().err().map(|e| format!("{:#}", e.error)),
        },
    );
    if json {
        let status = match status {
            RunStatus::Failed => "failed",
            RunStatus::Ok => "ok",
            RunStatus::Partial => "partial",
        };
        let mut payload = serde_json::json!({
            "schema_version": "anytopdf.convert/1",
            "status": status,
            "exit_code": exit_code,
            "profile": profile.as_str(),
            "outputs": outcome.outputs,
            "summary": {"converted": outcome.converted, "skipped": outcome.skipped},
            "diagnostics": outcome.diagnostics,
        });
        if let Err(e) = &result {
            payload["error"] = format!("{:#}", e.error).into();
        }
        println!("{}", serde_json::to_string_pretty(&payload)?);
    }
    result
}

#[derive(Default)]
struct ConvertOutcome {
    outputs: Vec<serde_json::Value>,
    converted: Vec<serde_json::Value>,
    skipped: Vec<serde_json::Value>,
    diagnostics: Vec<serde_json::Value>,
}

fn convert_inner(
    args: ConvertArgs,
    policy: &RuntimePluginPolicy,
    outcome: &mut ConvertOutcome,
    redactor: &mut Redactor,
    sink: &mut Sink,
) -> Result<(), CliError> {
    if policy.enabled {
        tag(
            ExitClass::Usage,
            anytopdf_core::validate_sandbox_policy(&policy.sandbox),
        )?;
    }
    let opts = BuiltinOptions {
        explicit_transcripts: args.transcripts.clone(),
        entities: !args.no_entities,
        date_order: tag(
            ExitClass::Usage,
            args.date_order.parse().map_err(anyhow::Error::msg),
        )?,
        colors: args.colors,
        chat: ChatOptions {
            attachments: !args.no_chat_attachments,
            date_order: args.chat_date_order,
        },
        raw_decode: args.raw_decode,
        scan: args.scan_mode,
        location: args.location,
        ..tag(
            ExitClass::Usage,
            BuiltinOptions::from_tables(&policy.options),
        )?
    };
    let video = &opts.video;
    if !video.interval.is_finite() || video.interval <= 0.0 {
        return Err(fail(
            ExitClass::Usage,
            "--video-interval must be finite and greater than zero",
        ));
    }
    if !video.scene_threshold.is_finite() || !(0.0..=1.0).contains(&video.scene_threshold) {
        return Err(fail(
            ExitClass::Usage,
            "--scene-threshold must be between 0 and 1",
        ));
    }
    if !args.face_threshold.is_finite() || !(-1.0..=1.0).contains(&args.face_threshold) {
        return Err(fail(
            ExitClass::Usage,
            "--face-threshold must be between -1 and 1",
        ));
    }
    if video.dedupe_distance > 64 {
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

    let fetched = crate::fetch::fetch_inputs(
        &args.inputs,
        &args.url.links,
        &args.url.options(),
        !args.quiet && sink.is_none(),
    )?;
    if let Some(dir) = &fetched.dir {
        redactor.add_dir(dir.path());
    }

    emit(
        sink,
        Event::StageStarted {
            stage: Stage::Discover.as_str(),
        },
    );
    let inputs = tag(
        ExitClass::Input,
        discover_inputs(
            &fetched.inputs,
            &DiscoveryOptions {
                include_hidden: args.include_hidden,
                filter,
            },
        ),
    )?;

    emit(
        sink,
        Event::StageFinished {
            stage: Stage::Discover.as_str(),
        },
    );

    // Keep command-line order (stable within a directory) so source order is predictable.
    let roots: Vec<PathBuf> = fetched
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
            .chain(&args.url.links)
            .map(|p| p.canonicalize())
            .collect::<std::io::Result<Vec<_>>>()?,
    );
    let single = if args.output_dir.is_some() {
        None
    } else {
        let out_path = match args.output.clone() {
            Some(path) => path,
            None => {
                let base = naming::default_output(&fetched.names, &std::env::current_dir()?);
                if args.overwrite {
                    base
                } else {
                    naming::next_free_path(&base, &protected)?
                }
            }
        };
        let output = tag(
            ExitClass::Usage,
            checked_destination(&out_path, &protected, args.overwrite),
        )?;
        Some((out_path, output))
    };
    let mut dump_dest = None;
    if let Some(path) = &args.dump_graph {
        let graph = tag(
            ExitClass::Usage,
            checked_destination(path, &protected, args.overwrite),
        )?;
        if single.as_ref().is_some_and(|(_, output)| graph == *output) {
            return Err(fail(
                ExitClass::Usage,
                "PDF and graph outputs must use different paths",
            ));
        }
        dump_dest = Some(graph);
    }

    let mut index = args
        .index
        .then(|| crate::search::open_for_convert(args.index_db.as_deref()))
        .transpose()?;

    let ocr_mode = opts.ocr.mode;
    let (registry, mut warnings) = registry(opts, policy);
    let pipeline = Pipeline::new(registry);
    let mut observer = EventObserver {
        sink,
        profile: args.profile,
    };
    let mut run = tag(
        ExitClass::Input,
        pipeline.ingest_observed(&inputs, args.quiet, &mut observer),
    )?;
    run.warnings.append(&mut warnings);
    run.warnings.extend(fetched.warnings.iter().cloned());
    let mut face_outcome = None;
    if args.recognize_faces {
        let index = crate::faces::index_path(args.face_index.as_deref())?;
        let (mut diagnostics, outcome) = crate::faces::recognize_run(
            &mut run.graph,
            &run.context.workspace,
            &index,
            args.face_threshold,
        );
        run.warnings.append(&mut diagnostics);
        face_outcome = outcome;
    } else {
        anytopdf_faces::recognize::strip_refs(&mut run.graph);
    }

    redactor.add_dir(&run.context.workspace.join("x"));
    for executable in anytopdf_core::runtime_plugin_candidates(policy) {
        redactor.add_parent_of(&executable);
    }
    let redactor = &*redactor;
    for diagnostic in &run.warnings {
        report_diagnostic(sink, diagnostic, redactor, args.profile);
    }

    let shown = |path: &Path| shown(args.profile, path);
    outcome.diagnostics = run
        .warnings
        .iter()
        .map(|d| {
            serde_json::json!({
                "code": d.code.as_str(),
                "severity": if d.severity == Severity::Info { "info" } else { "warning" },
                "message": redactor.apply(&d.message),
            })
        })
        .collect();
    let skipped: Vec<&Diagnostic> = run.warnings.iter().filter(|d| d.code.is_skip()).collect();
    outcome.skipped = skipped
        .iter()
        .map(|d| {
            serde_json::json!({
                "input": d.input.as_deref().map_or_else(String::new, shown),
                "code": d.code.as_str(),
                "message": redactor.apply(&d.message),
            })
        })
        .collect();
    if !skipped.is_empty() {
        if sink.is_none() {
            crate::exit::print_summary(run.graph.sources.len(), &skipped, redactor);
        }
        if args.fail_fast {
            return Err(fail(
                ExitClass::FailFast,
                "--fail-fast stopped on a skipped input",
            ));
        }
    }

    if let Some(d) = exhausted_provider(&run.warnings, ocr_mode) {
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
    if let Some(kinds) = &args.draw_boxes {
        metadata.insert(anytopdf_pdf::DRAW_BOXES_KEY.into(), kinds.clone());
    }
    let used = fetched.used_providers();
    let names: Vec<&'static str> = PROVIDER_NAMES
        .into_iter()
        .filter(|n| !URL_PROVIDERS.contains(n) || used.contains(n))
        .collect();
    for p in detect_providers_named(&names) {
        if let Some(version) = p.version {
            metadata.insert(format!("provider.{}.version", p.name), version);
        }
    }
    let job = crate::print::job_metadata(|key| std::env::var(key).ok());
    for source in &mut run.graph.sources {
        source.metadata.extend(job.iter().cloned());
    }
    let fetched_at = crate::queue::rfc3339(created.max(0) as u64);
    crate::fetch::annotate(&mut run.graph, &fetched.origins, &fetched_at);
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

    let source_paths: Vec<PathBuf> = run.graph.sources.iter().map(|s| s.path.clone()).collect();
    let whole = document;
    let mut docs: Vec<(Option<PathBuf>, DocumentGraph)> = Vec::new();
    if args.output_dir.is_some() {
        for (source, path) in whole.sources.iter().zip(&source_paths) {
            let units: Vec<_> = whole
                .units
                .iter()
                .filter(|u| u.source_id == source.id)
                .cloned()
                .collect();
            if units.is_empty() {
                continue;
            }
            docs.push((
                Some(path.clone()),
                DocumentGraph {
                    sources: vec![source.clone()],
                    units,
                    metadata: whole.metadata.clone(),
                },
            ));
        }
    } else {
        docs.push((None, whole));
    }

    // Render every document into staging before publishing any output.
    let mut staged_docs = Vec::new();
    emit(
        sink,
        Event::StageStarted {
            stage: Stage::Render.as_str(),
        },
    );
    for (i, (source_path, graph)) in docs.into_iter().enumerate() {
        run.graph = graph;
        let staged = run.context.workspace.join(format!("result-{i}.pdf"));
        let doc = stage_document(
            &pipeline,
            &run,
            (&args.renderer, args.strict, index.is_some()),
            &staged,
            redactor,
            sink,
            args.profile,
        )?;
        staged_docs.push((source_path, doc));
    }
    emit(
        sink,
        Event::StageFinished {
            stage: Stage::Render.as_str(),
        },
    );

    let mut published = Vec::new();
    if let Some(dir) = &args.output_dir {
        let dir = std::path::absolute(dir)?;
        let mut taken = protected.clone();
        taken.extend(dump_dest.clone());
        for (source_path, _) in &staged_docs {
            let stem = source_path.as_deref().and_then(Path::file_stem);
            let base = dir.join(format!(
                "{}.pdf",
                stem.unwrap_or_default().to_string_lossy()
            ));
            let path = naming::next_free(&base, &taken, !args.overwrite)?;
            taken.push(naming::resolve(&path)?);
            published.push(path);
        }
    } else if let Some((out_path, _)) = single {
        published.push(out_path);
    }

    for (out_path, (_, doc)) in published.iter().zip(&staged_docs) {
        publish_output(out_path, &doc.bytes, args.overwrite)?;
        emit(
            sink,
            Event::OutputWritten {
                path: shown(out_path),
                kind: "pdf",
                pages: Some(doc.pages),
            },
        );
        if let Some((manifest, chunks)) = &doc.sidecars {
            for (suffix, data) in [("manifest", manifest), ("chunks", chunks)] {
                let mut name = out_path.as_os_str().to_owned();
                name.push(format!(".{suffix}.json"));
                publish_output(Path::new(&name), data, args.overwrite)?;
                emit(
                    sink,
                    Event::OutputWritten {
                        path: shown(Path::new(&name)),
                        kind: suffix,
                        pages: None,
                    },
                );
            }
        }
    }
    if let Some(index) = &mut index {
        let docs = published
            .iter()
            .zip(&mut staged_docs)
            .filter_map(|(out, (_, doc))| Some((out.clone(), doc.index.take()?)))
            .collect();
        crate::search::record(index, args.collection.as_deref(), docs)?;
    }
    if let (Some(path), Some(graph)) = (&args.dump_graph, &dump) {
        publish_output(path, &serde_json::to_vec_pretty(graph)?, args.overwrite)?;
        emit(
            sink,
            Event::OutputWritten {
                path: shown(path),
                kind: "graph",
                pages: None,
            },
        );
    }

    for (out_path, (_, doc)) in published.iter().zip(&staged_docs) {
        outcome.outputs.push(serde_json::json!({
            "path": shown(out_path),
            "pages": doc.pages,
        }));
        outcome.converted.extend(doc.sources.clone());
        if !args.quiet && sink.is_none() {
            eprintln!("Wrote {} ({} pages)", redactor.path(out_path), doc.pages);
        }
    }
    if let Some(faces) = face_outcome.filter(|_| !args.quiet && sink.is_none()) {
        eprintln!(
            "Faces: {} recognized, {} new unnamed (see `anytopdf faces list`)",
            faces.recognized, faces.new_people
        );
    }
    Ok(())
}

pub(crate) fn print_diagnostic(d: &Diagnostic) {
    print_redacted(d, &Redactor::default());
}

pub(crate) fn print_redacted(d: &Diagnostic, redactor: &Redactor) {
    let level = match d.severity {
        Severity::Info => "INFO",
        Severity::Warning => "WARNING",
    };
    eprintln!(
        "{level} [{}]: {}",
        d.code.as_str(),
        redactor.apply(&d.message)
    );
}

fn exhausted_provider(warnings: &[Diagnostic], ocr: OcrMode) -> Option<&Diagnostic> {
    if matches!(ocr, OcrMode::Auto | OcrMode::Off) {
        return None;
    }
    warnings.iter().find(|d| d.provider_exhausted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_core::DiagnosticCode;

    fn diag(code: DiagnosticCode, message: &str, marked: bool) -> Diagnostic {
        let mut d = Diagnostic::new(code, message);
        d.provider_exhausted = marked;
        d
    }

    #[test]
    fn provider_class_comes_from_the_marker_not_the_message() {
        let text = "ocr unit enrichment failed: every OCR provider failed: x";
        let a = diag(DiagnosticCode::EnrichmentFailed, text, false);
        let b = diag(
            DiagnosticCode::ProviderFailed,
            "exiftool failed for a.jpg: 1",
            false,
        );
        let c = diag(DiagnosticCode::EnrichmentFailed, text, true);
        let d = c.clone();
        assert_eq!(
            exhausted_provider(&[a.clone(), b.clone()], OcrMode::Tesseract),
            None
        );
        assert_eq!(
            exhausted_provider(&[a, b, c.clone()], OcrMode::Tesseract),
            Some(&c)
        );
        assert_eq!(
            exhausted_provider(std::slice::from_ref(&d), OcrMode::Auto),
            None
        );
        assert_eq!(
            exhausted_provider(std::slice::from_ref(&d), OcrMode::Off),
            None
        );
    }
}
