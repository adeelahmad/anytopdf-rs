use crate::{
    Diagnostic, DiagnosticCode, DocumentGraph, JobContext, PipelineObserver, ProvidersExhausted,
    Registry, SourceRecord,
};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct Pipeline {
    pub registry: Registry,
}

pub struct PipelineRun {
    pub graph: DocumentGraph,
    pub warnings: Vec<Diagnostic>,
    _workspace_guard: TempDir,
    pub context: JobContext,
}

impl Pipeline {
    pub fn new(registry: Registry) -> Self {
        Self { registry }
    }

    // agentic:shim — emits nothing; GREEN moves the ingest body here
    pub fn ingest_observed(
        &self,
        paths: &[PathBuf],
        quiet: bool,
        _observer: &mut dyn PipelineObserver,
    ) -> Result<PipelineRun> {
        self.ingest(paths, quiet)
    }

    pub fn ingest(&self, paths: &[PathBuf], quiet: bool) -> Result<PipelineRun> {
        let workspace = tempfile::Builder::new()
            .prefix("anytopdf-")
            .tempdir()
            .context("create job workspace")?;

        let ctx = JobContext {
            workspace: workspace
                .path()
                .canonicalize()
                .context("resolve job workspace")?,
            quiet,
        };

        let mut graph = DocumentGraph::default();
        let mut warnings: Vec<Diagnostic> = Vec::new();

        for path in paths {
            let canonical = match path.canonicalize() {
                Ok(path) if path.is_file() => path,
                Ok(_) => {
                    warnings.push(Diagnostic::for_input(
                        DiagnosticCode::InputNotFile,
                        path,
                        format!("input is not a file: {}", path.display()),
                    ));
                    continue;
                }
                Err(e) => {
                    warnings.push(Diagnostic::for_input(
                        DiagnosticCode::InputUnreadable,
                        path,
                        format!("input {}: {e}", path.display()),
                    ));
                    continue;
                }
            };
            let mut source = SourceRecord::new(canonical);
            if let Some(kind) = infer::get_from_path(path).ok().flatten() {
                source.detected_type = Some(kind.mime_type().to_string());
            }

            for enricher in self.registry.source_enrichers() {
                if enricher.supports(&source) {
                    let original = source.clone();
                    match enricher.enrich_source(&ctx, &mut source) {
                        Ok(w) => warnings.extend(w.iter().map(|s| Diagnostic::from_wire(s))),
                        Err(e) => {
                            source = original;
                            warnings.push(Diagnostic::for_input(
                                DiagnosticCode::EnrichmentFailed,
                                path,
                                format!(
                                    "{} source enrichment failed for {}: {e:#}",
                                    enricher.descriptor().name,
                                    path.display()
                                ),
                            ));
                        }
                    }
                }
            }

            let importer = match self.registry.importer_for(&source) {
                Ok(p) => p,
                Err(e) => {
                    warnings.push(Diagnostic::for_input(
                        DiagnosticCode::InputUnsupported,
                        path,
                        e.to_string(),
                    ));
                    continue;
                }
            };

            match importer.import(&ctx, source.clone()) {
                Ok(outcome) => {
                    let candidate = DocumentGraph {
                        sources: vec![outcome.source.clone()],
                        units: outcome.units.clone(),
                        ..Default::default()
                    };
                    if let Err(e) = candidate.validate() {
                        warnings.push(Diagnostic::for_input(
                            DiagnosticCode::ImportInvalid,
                            path,
                            format!("invalid import from {}: {e:#}", path.display()),
                        ));
                        continue;
                    }
                    if graph.sources.iter().any(|s| s.id == outcome.source.id)
                        || outcome
                            .units
                            .iter()
                            .any(|u| graph.units.iter().any(|old| old.id == u.id))
                    {
                        warnings.push(Diagnostic::for_input(
                            DiagnosticCode::ImportDuplicateId,
                            path,
                            format!("duplicate import IDs from {}", path.display()),
                        ));
                        continue;
                    }
                    warnings.extend(outcome.warnings.iter().map(|s| Diagnostic::from_wire(s)));
                    graph.sources.push(outcome.source);
                    graph.units.extend(outcome.units);
                }
                Err(e) => warnings.push(Diagnostic::for_input(
                    DiagnosticCode::ImportFailed,
                    path,
                    format!(
                        "{} import failed for {}: {e:#}",
                        importer.descriptor().name,
                        path.display()
                    ),
                )),
            }
        }

        // Graph enrichers handle cross-unit semantics such as associating a
        // complete transcript with sampled video frames while also preserving a
        // full transcript unit for RAG extraction.
        for enricher in self.registry.graph_enrichers() {
            let original = graph.clone();
            match enricher
                .enrich_graph(&ctx, &mut graph)
                .and_then(|warnings| {
                    graph.validate()?;
                    Ok(warnings)
                }) {
                Ok(w) => warnings.extend(w.iter().map(|s| Diagnostic::from_wire(s))),
                Err(e) => {
                    graph = original;
                    warnings.push(Diagnostic::new(
                        DiagnosticCode::EnrichmentFailed,
                        format!(
                            "{} graph enrichment failed: {e:#}",
                            enricher.descriptor().name
                        ),
                    ));
                }
            }
        }

        // One consistent snapshot per provider avoids cloning the entire graph for every unit.
        for enricher in self.registry.unit_enrichers() {
            let snapshot = graph.clone();
            for unit in &mut graph.units {
                if enricher.supports(&snapshot, unit) {
                    let original = unit.clone();
                    match enricher
                        .enrich_unit(&ctx, &snapshot, unit)
                        .and_then(|warnings| {
                            anyhow::ensure!(
                                unit.id == original.id && unit.source_id == original.source_id,
                                "unit identity changed"
                            );
                            DocumentGraph {
                                sources: snapshot
                                    .source(unit.source_id)
                                    .cloned()
                                    .into_iter()
                                    .collect(),
                                units: vec![unit.clone()],
                                ..Default::default()
                            }
                            .validate()?;
                            Ok(warnings)
                        }) {
                        Ok(w) => warnings.extend(w.iter().map(|s| Diagnostic::from_wire(s))),
                        Err(e) => {
                            *unit = original;
                            let mut d = Diagnostic::new(
                                DiagnosticCode::EnrichmentFailed,
                                format!(
                                    "{} unit enrichment failed: {e:#}",
                                    enricher.descriptor().name
                                ),
                            );
                            d.provider_exhausted = e.chain().any(|c| c.is::<ProvidersExhausted>());
                            warnings.push(d);
                        }
                    }
                }
            }
        }

        graph.assign_content_ids()?;
        for unit in &mut graph.units {
            if unit.anchor.is_none()
                && let Some(source) = graph.sources.iter().find(|s| s.id == unit.source_id)
            {
                unit.anchor = Some(unit.default_anchor(source));
            }
        }

        Ok(PipelineRun {
            graph,
            warnings,
            _workspace_guard: workspace,
            context: ctx,
        })
    }

    pub fn render(
        &self,
        run: &PipelineRun,
        renderer_name: &str,
        output: &Path,
    ) -> Result<crate::RenderReport> {
        self.registry
            .renderer(renderer_name)?
            .render(&run.context, &run.graph, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;
    use std::sync::Arc;

    struct TextImport;
    impl Plugin for TextImport {
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor {
                name: "test-text".into(),
                version: "1".into(),
                kind: "importer".into(),
                extensions: vec![],
                mime_types: vec![],
                priority: 0,
            }
        }
    }
    impl Importer for TextImport {
        fn probe(&self, _: &SourceRecord) -> ProbeScore {
            ProbeScore::CERTAIN
        }
        fn import(&self, _: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
            let text = std::fs::read_to_string(&source.path)?;
            Ok(ImportOutcome {
                units: vec![Unit::text(source.id, text)],
                source,
                warnings: vec![],
            })
        }
    }
    struct BrokenEnricher;
    impl Plugin for BrokenEnricher {
        fn descriptor(&self) -> PluginDescriptor {
            TextImport.descriptor()
        }
    }
    impl UnitEnricher for BrokenEnricher {
        fn supports(&self, _: &DocumentGraph, _: &Unit) -> bool {
            true
        }
        fn enrich_unit(
            &self,
            _: &JobContext,
            _: &DocumentGraph,
            unit: &mut Unit,
        ) -> Result<Vec<String>> {
            unit.visible_text = Some("corrupted".into());
            anyhow::bail!("provider failed after mutation")
        }
    }
    impl GraphEnricher for BrokenEnricher {
        fn enrich_graph(&self, _: &JobContext, graph: &mut DocumentGraph) -> Result<Vec<String>> {
            graph.units.clear();
            anyhow::bail!("graph provider failed after mutation")
        }
    }

    struct Rec(Vec<PipelineEvent>);
    impl PipelineObserver for Rec {
        fn on_event(&mut self, event: &PipelineEvent) {
            self.0.push(event.clone());
        }
    }

    struct ExtImport {
        fail_on: Option<&'static str>,
    }
    impl Plugin for ExtImport {
        fn descriptor(&self) -> PluginDescriptor {
            TextImport.descriptor()
        }
    }
    impl Importer for ExtImport {
        fn probe(&self, source: &SourceRecord) -> ProbeScore {
            if source.path.extension().is_some_and(|e| e == "txt") {
                ProbeScore::CERTAIN
            } else {
                ProbeScore::NONE
            }
        }
        fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
            if self
                .fail_on
                .is_some_and(|n| source.path.file_name().is_some_and(|f| f == n))
            {
                anyhow::bail!("importer refused");
            }
            TextImport.import(ctx, source)
        }
    }

    struct NamedEnricher;
    impl Plugin for NamedEnricher {
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor {
                name: "test-enricher".into(),
                ..TextImport.descriptor()
            }
        }
    }
    impl UnitEnricher for NamedEnricher {
        fn supports(&self, _: &DocumentGraph, _: &Unit) -> bool {
            true
        }
        fn enrich_unit(
            &self,
            _: &JobContext,
            _: &DocumentGraph,
            _: &mut Unit,
        ) -> Result<Vec<String>> {
            Ok(vec![])
        }
    }

    fn observed_registry(fail_on: Option<&'static str>, enricher: bool) -> Registry {
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(ExtImport { fail_on }));
        if enricher {
            registry.register_unit_enricher(Arc::new(NamedEnricher));
        }
        registry
    }

    #[test]
    fn observer_sees_import_and_enrich_lifecycle_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let b = dir.path().join("b.bin");
        let c = dir.path().join("c.txt");
        std::fs::write(&a, "alpha").unwrap();
        std::fs::write(&b, [0u8; 16]).unwrap();
        std::fs::write(&c, "gamma").unwrap();
        let mut rec = Rec(vec![]);
        Pipeline::new(observed_registry(None, true))
            .ingest_observed(&[a.clone(), b.clone(), c.clone()], true, &mut rec)
            .unwrap();
        let e = || "test-enricher".to_string();
        let expected = vec![
            PipelineEvent::StageStarted(Stage::Import),
            PipelineEvent::SourceStarted {
                index: 0,
                path: a.clone(),
            },
            PipelineEvent::SourceImported {
                index: 0,
                path: a,
                units: 1,
            },
            PipelineEvent::SourceStarted {
                index: 1,
                path: b.clone(),
            },
            PipelineEvent::SourceSkipped {
                index: 1,
                path: b,
                code: DiagnosticCode::InputUnsupported,
            },
            PipelineEvent::SourceStarted {
                index: 2,
                path: c.clone(),
            },
            PipelineEvent::SourceImported {
                index: 2,
                path: c,
                units: 1,
            },
            PipelineEvent::StageFinished(Stage::Import),
            PipelineEvent::StageStarted(Stage::Enrich),
            PipelineEvent::UnitStarted {
                unit: 0,
                source: 0,
                enricher: e(),
            },
            PipelineEvent::UnitFinished {
                unit: 0,
                source: 0,
                enricher: e(),
            },
            PipelineEvent::UnitStarted {
                unit: 1,
                source: 1,
                enricher: e(),
            },
            PipelineEvent::UnitFinished {
                unit: 1,
                source: 1,
                enricher: e(),
            },
            PipelineEvent::StageFinished(Stage::Enrich),
        ];
        assert_eq!(rec.0, expected);
    }

    #[test]
    fn every_skipped_source_ends_with_source_skipped_and_its_code() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.txt");
        let sub = dir.path().join("dir");
        std::fs::create_dir(&sub).unwrap();
        let bad = dir.path().join("bad.txt");
        std::fs::write(&bad, "x").unwrap();
        let mut rec = Rec(vec![]);
        Pipeline::new(observed_registry(Some("bad.txt"), false))
            .ingest_observed(&[missing, sub, bad], true, &mut rec)
            .unwrap();
        let codes = [
            DiagnosticCode::InputUnreadable,
            DiagnosticCode::InputNotFile,
            DiagnosticCode::ImportFailed,
        ];
        for (index, want) in codes.into_iter().enumerate() {
            let started = rec
                .0
                .iter()
                .filter(
                    |ev| matches!(ev, PipelineEvent::SourceStarted { index: i, .. } if *i == index),
                )
                .count();
            let skipped: Vec<_> = rec
                .0
                .iter()
                .filter_map(|ev| match ev {
                    PipelineEvent::SourceSkipped { index: i, code, .. } if *i == index => {
                        Some(*code)
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(started, 1, "SourceStarted for {index}");
            assert_eq!(skipped, vec![want], "SourceSkipped for {index}");
        }
        assert!(
            !rec.0
                .iter()
                .any(|ev| matches!(ev, PipelineEvent::SourceImported { .. }))
        );
    }

    #[test]
    fn observed_and_plain_ingest_build_the_same_graph() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.txt");
        let c = dir.path().join("c.txt");
        std::fs::write(&a, "alpha").unwrap();
        std::fs::write(&c, "gamma").unwrap();
        let pipeline = Pipeline::new(observed_registry(None, true));
        let plain = pipeline.ingest(&[a.clone(), c.clone()], true).unwrap();
        let mut rec = Rec(vec![]);
        let observed = pipeline.ingest_observed(&[a, c], true, &mut rec).unwrap();
        assert_eq!(
            serde_json::to_string(&plain.graph).unwrap(),
            serde_json::to_string(&observed.graph).unwrap()
        );
        assert_eq!(plain.warnings.len(), observed.warnings.len());
        assert_eq!(
            format!("{:?}", plain.warnings),
            format!("{:?}", observed.warnings)
        );
        assert!(!rec.0.is_empty(), "observer received no events");
    }

    #[test]
    fn failed_enrichers_roll_back_and_workspace_lives_until_run_drops() {
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), "original").unwrap();
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(TextImport));
        registry.register_unit_enricher(Arc::new(BrokenEnricher));
        registry.register_graph_enricher(Arc::new(BrokenEnricher));
        let run = Pipeline::new(registry)
            .ingest(&[input.path().into()], true)
            .unwrap();
        assert_eq!(run.graph.units[0].visible_text.as_deref(), Some("original"));
        assert_eq!(run.warnings.len(), 2);
        let workspace = run.context.workspace.clone();
        assert!(workspace.is_dir());
        drop(run);
        assert!(!workspace.exists());
    }

    struct WarnImport;
    impl Plugin for WarnImport {
        fn descriptor(&self) -> PluginDescriptor {
            TextImport.descriptor()
        }
    }
    impl Importer for WarnImport {
        fn probe(&self, _: &SourceRecord) -> ProbeScore {
            ProbeScore::CERTAIN
        }
        fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
            let mut outcome = TextImport.import(ctx, source)?;
            outcome.warnings = vec![
                Diagnostic::new(DiagnosticCode::LossyDecode, "x").to_string(),
                "legacy".to_string(),
            ];
            Ok(outcome)
        }
    }

    #[test]
    fn unsupported_and_missing_inputs_are_coded_skips_with_paths() {
        let dir = tempfile::tempdir().unwrap();
        let existing = dir.path().join("exists.bin");
        std::fs::write(&existing, "data").unwrap();
        let missing = dir.path().join("missing.bin");
        let run = Pipeline::new(Registry::default())
            .ingest(&[existing.clone(), missing.clone()], true)
            .unwrap();
        let find = |code| run.warnings.iter().find(|d| d.code == code);
        let unsupported = find(DiagnosticCode::InputUnsupported).expect("unsupported diagnostic");
        assert_eq!(unsupported.input.as_deref(), Some(existing.as_path()));
        let unreadable = find(DiagnosticCode::InputUnreadable).expect("unreadable diagnostic");
        assert_eq!(unreadable.input.as_deref(), Some(missing.as_path()));
        for d in [unsupported, unreadable] {
            assert_eq!(d.severity, crate::Severity::Warning);
            assert!(d.code.is_skip());
        }
    }

    #[test]
    fn importer_wire_warnings_keep_their_codes() {
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), "text").unwrap();
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(WarnImport));
        let run = Pipeline::new(registry)
            .ingest(&[input.path().into()], true)
            .unwrap();
        assert!(
            run.warnings
                .iter()
                .any(|d| d.code == DiagnosticCode::LossyDecode && d.message == "x"),
            "{:?}",
            run.warnings
        );
        assert!(
            run.warnings
                .iter()
                .any(|d| d.code == DiagnosticCode::PluginWarning && d.message == "legacy"),
            "{:?}",
            run.warnings
        );
    }

    const IDENTITY_DIGEST: &str =
        "eea2ca13a1da285c9365c7dd3fdfb68eb34445313f8eb1b994991728ae3d917a";

    #[test]
    fn ingest_records_digest_size_and_derived_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id.txt");
        std::fs::write(&path, b"Identity fixture\n").unwrap();
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(TextImport));
        let run = Pipeline::new(registry).ingest(&[path], true).unwrap();
        let source = &run.graph.sources[0];
        assert_eq!(source.sha256.as_deref(), Some(IDENTITY_DIGEST));
        assert_eq!(source.size, Some(17));
        assert_eq!(source.id, content_source_id(IDENTITY_DIGEST, 0));
        assert_eq!(run.graph.units[0].id, content_unit_id(source.id, 0));
    }

    struct CaptionLike(PathBuf);
    impl Plugin for CaptionLike {
        fn descriptor(&self) -> PluginDescriptor {
            TextImport.descriptor()
        }
    }
    impl GraphEnricher for CaptionLike {
        fn enrich_graph(&self, _: &JobContext, graph: &mut DocumentGraph) -> Result<Vec<String>> {
            let source = SourceRecord::new(self.0.clone());
            graph.units.push(Unit::text(source.id, "caption".into()));
            graph.sources.push(source);
            Ok(vec![])
        }
    }

    #[test]
    fn caption_style_sources_added_by_enrichers_get_digests_and_derived_ids() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main.txt");
        let extra = dir.path().join("extra.txt");
        std::fs::write(&main, b"main bytes\n").unwrap();
        std::fs::write(&extra, b"Identity fixture\n").unwrap();
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(TextImport));
        registry.register_graph_enricher(Arc::new(CaptionLike(extra)));
        let run = Pipeline::new(registry).ingest(&[main], true).unwrap();
        let added = run.graph.sources.last().unwrap();
        assert_eq!(added.sha256.as_deref(), Some(IDENTITY_DIGEST));
        assert_eq!(added.size, Some(17));
        assert_eq!(added.id, content_source_id(IDENTITY_DIGEST, 0));
        let unit = run.graph.units.last().unwrap();
        assert_eq!(unit.source_id, added.id);
        assert_eq!(unit.id, content_unit_id(added.id, 0));
    }

    struct AnchoringImport;
    impl Plugin for AnchoringImport {
        fn descriptor(&self) -> PluginDescriptor {
            TextImport.descriptor()
        }
    }
    impl Importer for AnchoringImport {
        fn probe(&self, _: &SourceRecord) -> ProbeScore {
            ProbeScore::CERTAIN
        }
        fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
            let mut outcome = TextImport.import(ctx, source)?;
            outcome.units[0].anchor = Some(Anchor::Region {
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.4,
                frame: None,
            });
            Ok(outcome)
        }
    }

    #[test]
    fn every_ingested_unit_has_an_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id.txt");
        std::fs::write(&path, b"Identity fixture\n").unwrap();
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(TextImport));
        let run = Pipeline::new(registry).ingest(&[path], true).unwrap();
        assert_eq!(
            run.graph.units[0].anchor,
            Some(Anchor::ByteRange { start: 0, end: 17 })
        );
    }

    #[test]
    fn importer_supplied_anchor_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("id.txt");
        std::fs::write(&path, b"Identity fixture\n").unwrap();
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(AnchoringImport));
        let run = Pipeline::new(registry).ingest(&[path], true).unwrap();
        assert_eq!(
            run.graph.units[0].anchor,
            Some(Anchor::Region {
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.4,
                frame: None,
            })
        );
    }

    struct ExhaustedEnricher {
        typed: bool,
    }
    impl Plugin for ExhaustedEnricher {
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor {
                name: "ocr-test".into(),
                ..TextImport.descriptor()
            }
        }
    }
    impl UnitEnricher for ExhaustedEnricher {
        fn supports(&self, _: &DocumentGraph, _: &Unit) -> bool {
            true
        }
        fn enrich_unit(
            &self,
            _: &JobContext,
            _: &DocumentGraph,
            _: &mut Unit,
        ) -> Result<Vec<String>> {
            let message = "no OCR provider succeeded: x";
            if self.typed {
                Err(ProvidersExhausted {
                    message: message.into(),
                }
                .into())
            } else {
                Err(anyhow::anyhow!(message))
            }
        }
    }

    fn enrichment_failures(typed: bool) -> Vec<Diagnostic> {
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), "text").unwrap();
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(TextImport));
        registry.register_unit_enricher(Arc::new(ExhaustedEnricher { typed }));
        let run = Pipeline::new(registry)
            .ingest(&[input.path().into()], true)
            .unwrap();
        run.warnings
            .iter()
            .filter(|d| d.code == DiagnosticCode::EnrichmentFailed)
            .cloned()
            .collect()
    }

    #[test]
    fn typed_provider_exhaustion_is_marked_on_the_enrichment_diagnostic() {
        let failures = enrichment_failures(true);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].provider_exhausted);
        assert_eq!(
            failures[0].message,
            "ocr-test unit enrichment failed: no OCR provider succeeded: x"
        );
    }

    #[test]
    fn plain_error_with_the_same_text_is_not_marked() {
        let failures = enrichment_failures(false);
        assert_eq!(failures.len(), 1);
        assert!(!failures[0].provider_exhausted);
    }
}
