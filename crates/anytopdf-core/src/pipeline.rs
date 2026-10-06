use crate::{
    Diagnostic, DiagnosticCode, DocumentGraph, GraphPhase, ImportOutcome, JobContext,
    MemberImporter, PipelineEvent, PipelineObserver, ProvidersExhausted, Registry, SourceRecord,
    Stage,
};
use anyhow::{Context, Result};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct Pipeline {
    pub registry: Registry,
}

/// Containers nested deeper than this (an archive in an email in an archive...)
/// are not expanded further.
pub const MAX_MEMBER_DEPTH: usize = 4;

/// Members extracted for one top-level input, across all nesting levels.
pub const MAX_MEMBERS_PER_INPUT: u64 = 10_000;

/// Bytes extracted into the workspace for one top-level input, across all
/// nesting levels.
pub const MAX_MEMBER_BYTES_PER_INPUT: u64 = 2 * 1024 * 1024 * 1024;

#[derive(Default)]
struct MemberBudget {
    files: Cell<u64>,
    bytes: Cell<u64>,
}

struct RegistryMembers<'a> {
    registry: &'a Registry,
    depth: usize,
    budget: &'a MemberBudget,
}

impl MemberImporter for RegistryMembers<'_> {
    fn remaining_bytes(&self) -> u64 {
        MAX_MEMBER_BYTES_PER_INPUT.saturating_sub(self.budget.bytes.get())
    }

    fn charge(&self, bytes: u64) -> Result<()> {
        let files = self.budget.files.get() + 1;
        let total = self.budget.bytes.get().saturating_add(bytes);
        anyhow::ensure!(
            files <= MAX_MEMBERS_PER_INPUT,
            "more than {MAX_MEMBERS_PER_INPUT} members extracted from one input"
        );
        anyhow::ensure!(
            total <= MAX_MEMBER_BYTES_PER_INPUT,
            "members extracted from one input exceed {MAX_MEMBER_BYTES_PER_INPUT} bytes"
        );
        self.budget.files.set(files);
        self.budget.bytes.set(total);
        Ok(())
    }

    fn import_member(&self, ctx: &JobContext, path: &Path) -> Result<ImportOutcome> {
        anyhow::ensure!(
            self.depth < MAX_MEMBER_DEPTH,
            "containers are nested more than {MAX_MEMBER_DEPTH} deep"
        );
        let canonical = path
            .canonicalize()
            .with_context(|| format!("resolve member {}", path.display()))?;
        anyhow::ensure!(
            canonical.starts_with(&ctx.workspace) && canonical.is_file(),
            "member {} is not a file inside the job workspace",
            path.display()
        );
        let mut source = SourceRecord::new(canonical);
        if let Some(kind) = infer::get_from_path(&source.path).ok().flatten() {
            source.detected_type = Some(kind.mime_type().to_string());
        }
        let importer = self.registry.importer_for(&source)?;
        let nested = RegistryMembers {
            registry: self.registry,
            depth: self.depth + 1,
            budget: self.budget,
        };
        importer
            .import_with_members(ctx, source, &nested)
            .with_context(|| format!("{} import failed", importer.descriptor().name))
    }
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

    pub fn ingest(&self, paths: &[PathBuf], quiet: bool) -> Result<PipelineRun> {
        struct Silent;
        impl PipelineObserver for Silent {
            fn on_event(&mut self, _: &PipelineEvent) {}
        }
        self.ingest_observed(paths, quiet, &mut Silent)
    }

    pub fn ingest_observed(
        &self,
        paths: &[PathBuf],
        quiet: bool,
        observer: &mut dyn PipelineObserver,
    ) -> Result<PipelineRun> {
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

        observer.on_event(&PipelineEvent::StageStarted(Stage::Import));
        for (index, path) in paths.iter().enumerate() {
            observer.on_event(&PipelineEvent::SourceStarted {
                index,
                path: path.clone(),
            });
            let skip = |observer: &mut dyn PipelineObserver, code| {
                observer.on_event(&PipelineEvent::SourceSkipped {
                    index,
                    path: path.clone(),
                    code,
                })
            };
            let canonical = match path.canonicalize() {
                Ok(path) if path.is_file() => path,
                Ok(_) => {
                    warnings.push(Diagnostic::for_input(
                        DiagnosticCode::InputNotFile,
                        path,
                        format!("input is not a file: {}", path.display()),
                    ));
                    skip(observer, DiagnosticCode::InputNotFile);
                    continue;
                }
                Err(e) => {
                    warnings.push(Diagnostic::for_input(
                        DiagnosticCode::InputUnreadable,
                        path,
                        format!("input {}: {e}", path.display()),
                    ));
                    skip(observer, DiagnosticCode::InputUnreadable);
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
                    skip(observer, DiagnosticCode::InputUnsupported);
                    continue;
                }
            };

            let budget = MemberBudget::default();
            let members = RegistryMembers {
                registry: &self.registry,
                depth: 0,
                budget: &budget,
            };
            match importer.import_with_members(&ctx, source.clone(), &members) {
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
                        skip(observer, DiagnosticCode::ImportInvalid);
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
                        skip(observer, DiagnosticCode::ImportDuplicateId);
                        continue;
                    }
                    warnings.extend(outcome.warnings.iter().map(|s| Diagnostic::from_wire(s)));
                    observer.on_event(&PipelineEvent::SourceImported {
                        index,
                        path: path.clone(),
                        units: outcome.units.len(),
                    });
                    graph.sources.push(outcome.source);
                    graph.units.extend(outcome.units);
                }
                Err(e) => {
                    warnings.push(Diagnostic::for_input(
                        DiagnosticCode::ImportFailed,
                        path,
                        format!(
                            "{} import failed for {}: {e:#}",
                            importer.descriptor().name,
                            path.display()
                        ),
                    ));
                    skip(observer, DiagnosticCode::ImportFailed);
                }
            }
        }

        observer.on_event(&PipelineEvent::StageFinished(Stage::Import));
        observer.on_event(&PipelineEvent::StageStarted(Stage::Enrich));

        // Graph enrichers handle cross-unit semantics such as associating a
        // complete transcript with sampled video frames while also preserving a
        // full transcript unit for RAG extraction.
        self.enrich_graph_phase(&ctx, &mut graph, &mut warnings, GraphPhase::BeforeUnits);

        // One consistent snapshot per provider avoids cloning the entire graph for every unit.
        for enricher in self.registry.unit_enrichers() {
            let snapshot = graph.clone();
            for (unit_index, unit) in graph.units.iter_mut().enumerate() {
                if enricher.supports(&snapshot, unit) {
                    let source_index = snapshot
                        .sources
                        .iter()
                        .position(|s| s.id == unit.source_id)
                        .unwrap_or_default();
                    let name = enricher.descriptor().name;
                    observer.on_event(&PipelineEvent::UnitStarted {
                        unit: unit_index,
                        source: source_index,
                        enricher: name.clone(),
                    });
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
                    observer.on_event(&PipelineEvent::UnitFinished {
                        unit: unit_index,
                        source: source_index,
                        enricher: name,
                    });
                }
            }
        }

        // Late graph enrichers summarize what the unit enrichers found.
        self.enrich_graph_phase(&ctx, &mut graph, &mut warnings, GraphPhase::AfterUnits);

        observer.on_event(&PipelineEvent::StageFinished(Stage::Enrich));
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

    fn enrich_graph_phase(
        &self,
        ctx: &JobContext,
        graph: &mut DocumentGraph,
        warnings: &mut Vec<Diagnostic>,
        phase: GraphPhase,
    ) {
        for enricher in self.registry.graph_enrichers() {
            if enricher.phase() != phase {
                continue;
            }
            let original = graph.clone();
            match enricher.enrich_graph(ctx, graph).and_then(|warnings| {
                graph.validate()?;
                Ok(warnings)
            }) {
                Ok(w) => warnings.extend(w.iter().map(|s| Diagnostic::from_wire(s))),
                Err(e) => {
                    *graph = original;
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

    /// Appends a marker to the text unit so the test can read the run order.
    struct Mark(&'static str, GraphPhase);
    impl Plugin for Mark {
        fn descriptor(&self) -> PluginDescriptor {
            TextImport.descriptor()
        }
    }
    impl UnitEnricher for Mark {
        fn supports(&self, _: &DocumentGraph, _: &Unit) -> bool {
            true
        }
        fn enrich_unit(
            &self,
            _: &JobContext,
            _: &DocumentGraph,
            unit: &mut Unit,
        ) -> Result<Vec<String>> {
            unit.visible_text.get_or_insert_default().push_str(self.0);
            Ok(vec![])
        }
    }
    impl GraphEnricher for Mark {
        fn enrich_graph(&self, _: &JobContext, graph: &mut DocumentGraph) -> Result<Vec<String>> {
            for unit in &mut graph.units {
                unit.visible_text.get_or_insert_default().push_str(self.0);
            }
            Ok(vec![])
        }
        fn phase(&self) -> GraphPhase {
            self.1
        }
    }

    #[test]
    fn after_units_graph_enrichers_run_after_unit_enrichers() {
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), "").unwrap();
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(TextImport));
        registry.register_graph_enricher(Arc::new(Mark("L", GraphPhase::AfterUnits)));
        registry.register_unit_enricher(Arc::new(Mark("U", GraphPhase::BeforeUnits)));
        registry.register_graph_enricher(Arc::new(Mark("E", GraphPhase::BeforeUnits)));
        let run = Pipeline::new(registry)
            .ingest(&[input.path().into()], true)
            .unwrap();
        assert!(run.warnings.is_empty(), "{:?}", run.warnings);
        assert_eq!(run.graph.units[0].visible_text.as_deref(), Some("EUL"));
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

    /// Expands into members according to the input text: `outside` asks for a
    /// file outside the workspace, `budget` charges past the per-input limits.
    struct ProbeContainer;
    impl Plugin for ProbeContainer {
        fn descriptor(&self) -> PluginDescriptor {
            TextImport.descriptor()
        }
    }
    impl Importer for ProbeContainer {
        fn probe(&self, _: &SourceRecord) -> ProbeScore {
            ProbeScore::CERTAIN
        }
        fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
            self.import_with_members(ctx, source, &NoMembers)
        }
        fn import_with_members(
            &self,
            ctx: &JobContext,
            source: SourceRecord,
            members: &dyn MemberImporter,
        ) -> Result<ImportOutcome> {
            let mode = std::fs::read_to_string(&source.path)?;
            let mut warnings = Vec::new();
            if mode == "outside" {
                let err = members.import_member(ctx, &source.path).unwrap_err();
                warnings.push(format!("{err:#}"));
            } else {
                for _ in 0..MAX_MEMBERS_PER_INPUT {
                    members.charge(0)?;
                }
                warnings.push(format!("{:#}", members.charge(0).unwrap_err()));
                members.charge(u64::MAX).unwrap_err();
            }
            Ok(ImportOutcome {
                units: vec![Unit::text(source.id, mode)],
                source,
                warnings,
            })
        }
    }

    #[test]
    fn members_must_live_in_the_workspace_and_respect_the_input_budget() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(ProbeContainer));
        let pipeline = Pipeline::new(registry);
        let outside = dir.path().join("a");
        let budget = dir.path().join("b");
        std::fs::write(&outside, "outside").unwrap();
        std::fs::write(&budget, "budget").unwrap();
        let run = pipeline.ingest(&[outside, budget], true).unwrap();
        let messages: Vec<&str> = run.warnings.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages.len(), 2, "{messages:?}");
        assert!(
            messages[0].contains("not a file inside the job workspace"),
            "{}",
            messages[0]
        );
        assert!(
            messages[1].contains("more than 10000 members"),
            "{}",
            messages[1]
        );
        assert_eq!(run.graph.units.len(), 2);
    }
}
