use crate::{Anchor, DocumentGraph, PageRange, RenderReport, Unit, UnitKind, Uuid};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MANIFEST_SCHEMA_VERSION: &str = "anytopdf.manifest/1";
pub const CHUNKS_SCHEMA_VERSION: &str = "anytopdf.chunks/1";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestGenerator {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestSource {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    pub sha256: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<PageRange>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestUnit {
    pub id: Uuid,
    pub source_id: Uuid,
    pub kind: UnitKind,
    pub anchor: Anchor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<PageRange>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: String,
    pub generator: ManifestGenerator,
    pub profile: String,
    pub created: Option<String>,
    pub providers: Vec<ProviderEntry>,
    pub sources: Vec<ManifestSource>,
    pub units: Vec<ManifestUnit>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub id: Uuid,
    pub source_id: Uuid,
    pub kind: UnitKind,
    pub text: String,
    pub anchor: Anchor,
    pub pages: PageRange,
    pub providers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkSet {
    pub schema_version: String,
    pub chunks: Vec<Chunk>,
}

fn page_span(report: &RenderReport, units: &[&Unit]) -> Option<PageRange> {
    units
        .iter()
        .filter_map(|u| report.unit_pages.get(&u.id))
        .copied()
        .reduce(|a, b| PageRange {
            first: a.first.min(b.first),
            last: a.last.max(b.last),
        })
}

fn unit_anchor(graph: &DocumentGraph, unit: &Unit) -> Option<Anchor> {
    unit.anchor.clone().or_else(|| {
        let source = graph.sources.iter().find(|s| s.id == unit.source_id)?;
        Some(unit.default_anchor(source))
    })
}

impl Manifest {
    pub fn build(graph: &DocumentGraph, report: &RenderReport) -> Self {
        let meta = |key: &str| graph.metadata.get(key).cloned();
        let providers = graph
            .metadata
            .iter()
            .filter_map(|(k, v)| {
                let name = k.strip_prefix("provider.")?.strip_suffix(".version")?;
                Some(ProviderEntry {
                    name: name.into(),
                    version: v.clone(),
                })
            })
            .collect();
        let sources = graph
            .sources
            .iter()
            .map(|s| {
                let units: Vec<&Unit> =
                    graph.units.iter().filter(|u| u.source_id == s.id).collect();
                ManifestSource {
                    id: s.id,
                    name: s
                        .path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    path: None,
                    media_type: s.detected_type.clone(),
                    sha256: s.sha256.clone().unwrap_or_default(),
                    size: s.size.unwrap_or_default(),
                    pages: page_span(report, &units),
                    metadata: s.metadata.clone(),
                }
            })
            .collect();
        let units = graph
            .units
            .iter()
            .filter_map(|u| {
                Some(ManifestUnit {
                    id: u.id,
                    source_id: u.source_id,
                    kind: u.kind,
                    anchor: unit_anchor(graph, u)?,
                    pages: report.unit_pages.get(&u.id).copied(),
                })
            })
            .collect();
        Self {
            schema_version: MANIFEST_SCHEMA_VERSION.into(),
            generator: ManifestGenerator {
                name: "anytopdf".into(),
                version: meta("anytopdf.version")
                    .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into()),
            },
            profile: meta("anytopdf.profile").unwrap_or_default(),
            created: meta("anytopdf.created"),
            providers,
            sources,
            units,
        }
    }
}

impl ChunkSet {
    pub fn build(graph: &DocumentGraph, report: &RenderReport) -> Self {
        let chunks = graph
            .units
            .iter()
            .filter_map(|u| {
                let mut providers: Vec<String> = Vec::new();
                for a in &u.annotations {
                    if !providers.contains(&a.provider) {
                        providers.push(a.provider.clone());
                    }
                }
                let text = u.visible_text.clone().unwrap_or_else(|| {
                    u.annotations
                        .iter()
                        .map(|a| a.text.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                });
                Some(Chunk {
                    id: u.id,
                    source_id: u.source_id,
                    kind: u.kind,
                    text,
                    anchor: unit_anchor(graph, u)?,
                    pages: *report.unit_pages.get(&u.id)?,
                    providers,
                })
            })
            .collect();
        Self {
            schema_version: CHUNKS_SCHEMA_VERSION.into(),
            chunks,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Annotation, AnnotationKind, SourceRecord, Unit};
    use serde_json::{Value, json};

    fn source(name: &str, digest: &str, size: u64) -> SourceRecord {
        let mut s = SourceRecord::new(format!("/work/{name}").into());
        s.detected_type = Some("text/plain".into());
        s.sha256 = Some(digest.into());
        s.size = Some(size);
        s
    }

    fn unit(source: &SourceRecord, start: u64, end: u64) -> Unit {
        let mut u = Unit::text(source.id, "visible words".into());
        u.anchor = Some(Anchor::ByteRange { start, end });
        u
    }

    fn range(first: usize, last: usize) -> PageRange {
        PageRange { first, last }
    }

    fn report(pairs: &[(Uuid, PageRange)]) -> RenderReport {
        RenderReport {
            pages: 9,
            warnings: Vec::new(),
            unit_pages: pairs.iter().copied().collect(),
        }
    }

    fn two_source_fixture() -> (DocumentGraph, RenderReport) {
        let (a, b) = (source("a.txt", "aa11", 10), source("b.txt", "bb22", 20));
        let (a1, a2, b1) = (unit(&a, 0, 5), unit(&a, 5, 10), unit(&b, 0, 20));
        let report = report(&[
            (a1.id, range(1, 1)),
            (a2.id, range(2, 3)),
            (b1.id, range(4, 4)),
        ]);
        let graph = DocumentGraph {
            sources: vec![a, b],
            units: vec![a1, a2, b1],
            ..Default::default()
        };
        (graph, report)
    }

    fn read_schema(name: &str) -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../schemas")
            .join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("schema {} must be committed: {e}", path.display()));
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} must parse: {e}"))
    }

    fn valid_pair() -> (Manifest, ChunkSet) {
        let (graph, report) = two_source_fixture();
        (
            Manifest::build(&graph, &report),
            ChunkSet::build(&graph, &report),
        )
    }

    #[test]
    fn manifest_records_each_source_with_digest_size_type_and_pages() {
        let (graph, report) = two_source_fixture();
        let manifest = Manifest::build(&graph, &report);
        assert_eq!(manifest.schema_version, "anytopdf.manifest/1");
        assert_eq!(manifest.sources.len(), 2);
        for (record, entry) in graph.sources.iter().zip(&manifest.sources) {
            assert_eq!(entry.id, record.id);
            assert_eq!(Some(&entry.sha256), record.sha256.as_ref());
            assert_eq!(Some(entry.size), record.size);
            assert_eq!(entry.media_type, record.detected_type);
            assert_eq!(
                Some(entry.name.as_str()),
                record.path.file_name().and_then(|n| n.to_str())
            );
        }
        assert_eq!(manifest.sources[0].pages, Some(range(1, 3)));
        assert_eq!(manifest.sources[1].pages, Some(range(4, 4)));
        assert_eq!(manifest.units.len(), graph.units.len());
        for (unit, entry) in graph.units.iter().zip(&manifest.units) {
            assert_eq!(entry.id, unit.id);
            assert_eq!(entry.source_id, unit.source_id);
            assert_eq!(Some(&entry.anchor), unit.anchor.as_ref());
            assert_eq!(entry.pages, report.unit_pages.get(&unit.id).copied());
        }
    }

    #[test]
    fn manifest_records_tool_and_provider_versions_and_profile() {
        let (mut graph, report) = two_source_fixture();
        for (k, v) in [
            ("anytopdf.version", "0.1.0"),
            ("anytopdf.profile", "share"),
            ("anytopdf.created", "1700000000"),
            ("provider.tesseract.version", "5.5.0"),
            ("provider.exiftool.version", "13.25"),
        ] {
            graph.metadata.insert(k.into(), v.into());
        }
        let manifest = Manifest::build(&graph, &report);
        let provider = |n: &str, v: &str| ProviderEntry {
            name: n.into(),
            version: v.into(),
        };
        assert_eq!(manifest.generator.version, "0.1.0");
        assert_eq!(manifest.profile, "share");
        assert_eq!(manifest.created, Some("1700000000".to_string()));
        assert_eq!(
            manifest.providers,
            vec![
                provider("exiftool", "13.25"),
                provider("tesseract", "5.5.0")
            ]
        );
    }

    #[test]
    fn every_chunk_traces_to_source_unit_anchor_pages_and_provider() {
        let a = source("a.txt", "aa11", 10);
        let text = unit(&a, 0, 10);
        let mut visual = Unit::visual(a.id, "scan.png".into());
        visual.anchor = Some(Anchor::Region {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        });
        visual.annotations.push(Annotation::text(
            AnnotationKind::Ocr,
            "tesseract",
            "OCR words",
        ));
        let report = report(&[(text.id, range(1, 1)), (visual.id, range(2, 2))]);
        let graph = DocumentGraph {
            sources: vec![a],
            units: vec![text, visual],
            ..Default::default()
        };
        let manifest = Manifest::build(&graph, &report);
        let chunks = ChunkSet::build(&graph, &report);
        assert_eq!(chunks.schema_version, "anytopdf.chunks/1");
        assert_eq!(chunks.chunks.len(), 2);
        for (unit, chunk) in graph.units.iter().zip(&chunks.chunks) {
            assert_eq!(chunk.id, unit.id);
            assert!(manifest.sources.iter().any(|s| s.id == chunk.source_id));
            assert_eq!(Some(&chunk.anchor), unit.anchor.as_ref());
            assert_eq!(Some(&chunk.pages), report.unit_pages.get(&unit.id));
        }
        assert_eq!(chunks.chunks[1].text, "OCR words");
        assert_eq!(chunks.chunks[1].providers, vec!["tesseract".to_string()]);
    }

    #[test]
    fn manifest_and_chunks_validate_against_committed_schemas() {
        let (manifest, chunks) = valid_pair();
        let (ms, cs) = (
            read_schema("manifest.schema.json"),
            read_schema("chunks.schema.json"),
        );
        for schema in [&ms, &cs] {
            assert!(schema.get("$schema").is_some(), "schema declares $schema");
            assert!(schema.get("$id").is_some(), "schema declares $id");
        }
        let errors = crate::schema::validate(&ms, &serde_json::to_value(&manifest).unwrap());
        assert!(errors.is_empty(), "manifest invalid: {errors:?}");
        let errors = crate::schema::validate(&cs, &serde_json::to_value(&chunks).unwrap());
        assert!(errors.is_empty(), "chunks invalid: {errors:?}");
    }

    #[test]
    fn schemas_reject_missing_digest_and_wrong_version() {
        let (manifest, chunks) = valid_pair();
        let (ms, cs) = (
            read_schema("manifest.schema.json"),
            read_schema("chunks.schema.json"),
        );
        let base = serde_json::to_value(&manifest).unwrap();
        assert!(
            base["sources"][0].get("sha256").is_some(),
            "fixture has digest"
        );
        let mut no_digest = base.clone();
        no_digest["sources"][0]
            .as_object_mut()
            .unwrap()
            .remove("sha256");
        assert!(!crate::schema::validate(&ms, &no_digest).is_empty());
        let mut wrong = base;
        wrong["schema_version"] = json!("anytopdf.manifest/2");
        assert!(!crate::schema::validate(&ms, &wrong).is_empty());
        let mut no_source = serde_json::to_value(&chunks).unwrap();
        assert!(
            no_source["chunks"][0].get("source_id").is_some(),
            "fixture has chunk"
        );
        no_source["chunks"][0]
            .as_object_mut()
            .unwrap()
            .remove("source_id");
        assert!(!crate::schema::validate(&cs, &no_source).is_empty());
    }
}
