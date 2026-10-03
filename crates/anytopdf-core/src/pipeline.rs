use crate::{DocumentGraph, JobContext, Registry, SourceRecord};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

pub struct Pipeline {
    pub registry: Registry,
}

pub struct PipelineRun {
    pub graph: DocumentGraph,
    pub warnings: Vec<String>,
    _workspace_guard: TempDir,
    pub context: JobContext,
}

impl Pipeline {
    pub fn new(registry: Registry) -> Self {
        Self { registry }
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
        let mut warnings = Vec::new();

        for path in paths {
            let canonical = match path.canonicalize() {
                Ok(path) if path.is_file() => path,
                Ok(_) => {
                    warnings.push(format!("input is not a file: {}", path.display()));
                    continue;
                }
                Err(e) => {
                    warnings.push(format!("input {}: {e}", path.display()));
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
                        Ok(mut w) => warnings.append(&mut w),
                        Err(e) => {
                            source = original;
                            warnings.push(format!(
                                "{} source enrichment failed for {}: {e:#}",
                                enricher.descriptor().name,
                                path.display()
                            ));
                        }
                    }
                }
            }

            let importer = match self.registry.importer_for(&source) {
                Ok(p) => p,
                Err(e) => {
                    warnings.push(e.to_string());
                    continue;
                }
            };

            match importer.import(&ctx, source.clone()) {
                Ok(mut outcome) => {
                    let candidate = DocumentGraph {
                        sources: vec![outcome.source.clone()],
                        units: outcome.units.clone(),
                        ..Default::default()
                    };
                    if let Err(e) = candidate.validate() {
                        warnings.push(format!("invalid import from {}: {e:#}", path.display()));
                        continue;
                    }
                    if graph.sources.iter().any(|s| s.id == outcome.source.id)
                        || outcome
                            .units
                            .iter()
                            .any(|u| graph.units.iter().any(|old| old.id == u.id))
                    {
                        warnings.push(format!("duplicate import IDs from {}", path.display()));
                        continue;
                    }
                    warnings.append(&mut outcome.warnings);
                    graph.sources.push(outcome.source);
                    graph.units.extend(outcome.units);
                }
                Err(e) => warnings.push(format!(
                    "{} import failed for {}: {e:#}",
                    importer.descriptor().name,
                    path.display()
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
                Ok(mut w) => warnings.append(&mut w),
                Err(e) => {
                    graph = original;
                    warnings.push(format!(
                        "{} graph enrichment failed: {e:#}",
                        enricher.descriptor().name
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
                        Ok(mut w) => warnings.append(&mut w),
                        Err(e) => {
                            *unit = original;
                            warnings.push(format!(
                                "{} unit enrichment failed: {e:#}",
                                enricher.descriptor().name
                            ));
                        }
                    }
                }
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
}
