use crate::{
    GraphEnricherRef, ImporterRef, PluginDescriptor, ProbeScore, RendererRef, SourceEnricherRef,
    SourceRecord, UnitEnricherRef,
};
use anyhow::Result;

#[derive(Default)]
pub struct Registry {
    importers: Vec<ImporterRef>,
    source_enrichers: Vec<SourceEnricherRef>,
    graph_enrichers: Vec<GraphEnricherRef>,
    unit_enrichers: Vec<UnitEnricherRef>,
    renderers: Vec<RendererRef>,
}

impl Registry {
    pub fn register_importer(&mut self, p: ImporterRef) {
        self.importers.push(p);
    }

    pub fn register_source_enricher(&mut self, p: SourceEnricherRef) {
        self.source_enrichers.push(p);
    }

    pub fn register_graph_enricher(&mut self, p: GraphEnricherRef) {
        let at = self
            .graph_enrichers
            .partition_point(|e| e.order() <= p.order());
        self.graph_enrichers.insert(at, p);
    }

    pub fn register_unit_enricher(&mut self, p: UnitEnricherRef) {
        let at = self
            .unit_enrichers
            .partition_point(|e| e.order() <= p.order());
        self.unit_enrichers.insert(at, p);
    }

    pub fn register_renderer(&mut self, p: RendererRef) {
        self.renderers.push(p);
    }

    pub fn importer_for(&self, source: &SourceRecord) -> Result<ImporterRef> {
        let mut candidates: Vec<(ProbeScore, i32, ImporterRef)> = self
            .importers
            .iter()
            .cloned()
            .map(|p| {
                let score = p.probe(source);
                let priority = p.descriptor().priority;
                (score, priority, p)
            })
            .filter(|(score, _, _)| *score > ProbeScore::NONE)
            .collect();

        candidates.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));

        candidates
            .into_iter()
            .next()
            .map(|(_, _, p)| p)
            .ok_or_else(|| anyhow::anyhow!("no importer matched {}", source.path.display()))
    }

    pub fn source_enrichers(&self) -> &[SourceEnricherRef] {
        &self.source_enrichers
    }

    pub fn graph_enrichers(&self) -> &[GraphEnricherRef] {
        &self.graph_enrichers
    }

    pub fn unit_enrichers(&self) -> &[UnitEnricherRef] {
        &self.unit_enrichers
    }

    pub fn renderer(&self, name: &str) -> Result<RendererRef> {
        self.renderers
            .iter()
            .find(|p| p.descriptor().name == name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("renderer not registered: {name}"))
    }

    pub fn descriptors(&self) -> Vec<PluginDescriptor> {
        let mut out = Vec::new();
        out.extend(self.importers.iter().map(|p| p.descriptor()));
        out.extend(self.source_enrichers.iter().map(|p| p.descriptor()));
        out.extend(self.graph_enrichers.iter().map(|p| p.descriptor()));
        out.extend(self.unit_enrichers.iter().map(|p| p.descriptor()));
        out.extend(self.renderers.iter().map(|p| p.descriptor()));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ImportOutcome, Importer, JobContext, Plugin};
    use std::sync::Arc;

    struct Candidate(&'static str, ProbeScore, i32);
    impl Plugin for Candidate {
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor {
                name: self.0.into(),
                version: "1".into(),
                kind: "importer".into(),
                extensions: vec![],
                mime_types: vec![],
                priority: self.2,
            }
        }
    }
    impl Importer for Candidate {
        fn probe(&self, _: &SourceRecord) -> ProbeScore {
            self.1
        }
        fn import(&self, _: &JobContext, _: SourceRecord) -> Result<ImportOutcome> {
            unreachable!()
        }
    }

    #[test]
    fn score_precedes_priority_and_priority_breaks_ties() {
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(Candidate(
            "extension",
            ProbeScore::EXTENSION,
            1000,
        )));
        registry.register_importer(Arc::new(Candidate("magic", ProbeScore::MAGIC, 1)));
        registry.register_importer(Arc::new(Candidate("magic-priority", ProbeScore::MAGIC, 2)));
        let source = SourceRecord::new("file.bin".into());
        assert_eq!(
            registry.importer_for(&source).unwrap().descriptor().name,
            "magic-priority"
        );
    }

    #[test]
    fn zero_score_is_never_selected() {
        let mut registry = Registry::default();
        registry.register_importer(Arc::new(Candidate("none", ProbeScore::NONE, 1000)));
        assert!(
            registry
                .importer_for(&SourceRecord::new("x".into()))
                .is_err()
        );
    }

    struct Ordered(&'static str, i32);
    impl Plugin for Ordered {
        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor {
                name: self.0.into(),
                version: "1".into(),
                kind: "unit-enricher".into(),
                extensions: vec![],
                mime_types: vec![],
                priority: 0,
            }
        }
        fn order(&self) -> i32 {
            self.1
        }
    }
    impl crate::UnitEnricher for Ordered {
        fn supports(&self, _: &crate::DocumentGraph, _: &crate::Unit) -> bool {
            true
        }
        fn enrich_unit(
            &self,
            _: &JobContext,
            _: &crate::DocumentGraph,
            _: &mut crate::Unit,
        ) -> Result<Vec<String>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn enrichers_run_by_order_then_registration() {
        let mut registry = Registry::default();
        for (name, order) in [("recognize", 100), ("detect", 0), ("ocr", 0), ("early", -5)] {
            registry.register_unit_enricher(Arc::new(Ordered(name, order)));
        }
        let names: Vec<String> = registry
            .unit_enrichers()
            .iter()
            .map(|e| e.descriptor().name)
            .collect();
        assert_eq!(names, ["early", "detect", "ocr", "recognize"]);
    }
}
