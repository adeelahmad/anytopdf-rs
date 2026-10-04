use anyhow::Result;
use anytopdf_core::*;
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
    fn enrich_unit(&self, _: &JobContext, _: &DocumentGraph, _: &mut Unit) -> Result<Vec<String>> {
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
            .filter(|ev| matches!(ev, PipelineEvent::SourceStarted { index: i, .. } if *i == index))
            .count();
        let skipped: Vec<_> = rec
            .0
            .iter()
            .filter_map(|ev| match ev {
                PipelineEvent::SourceSkipped { index: i, code, .. } if *i == index => Some(*code),
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
