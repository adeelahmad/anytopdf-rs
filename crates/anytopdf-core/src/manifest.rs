use crate::{
    Anchor, Annotation, AnnotationKind, DocumentGraph, PageRange, RenderReport, Unit, UnitKind,
    Uuid,
};
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
    /// Structured entities (URLs, emails, domains, app names, dates, times)
    /// found in the unit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<ChunkEntity>,
    /// Positioned words of a visual unit (OCR or the source's own text layer), in
    /// reading order, with boxes normalized to the unit's page.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<ChunkWord>,
}

/// One word on a page: `x` and `y` are the top-left corner and every value is a
/// fraction of the page width or height.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkWord {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkEntity {
    pub kind: String,
    pub value: String,
}

/// The entity kind of an annotation that records one (`attributes.entity`).
pub fn annotation_entity(a: &Annotation) -> Option<&str> {
    a.attributes.get("entity").map(String::as_str)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChunkSet {
    pub schema_version: String,
    pub chunks: Vec<Chunk>,
}

/// Annotation texts, one per line, except that consecutive word-level OCR boxes on
/// the same visual line are joined with spaces, so OCR chunks read as lines of text.
fn annotation_text<'a>(annotations: impl Iterator<Item = &'a Annotation>) -> String {
    let mut text = String::new();
    let mut previous: Option<&Annotation> = None;
    for a in annotations {
        if let Some(p) = previous {
            text.push(if same_ocr_line(p, a) { ' ' } else { '\n' });
        }
        text.push_str(&a.text);
        previous = Some(a);
    }
    text
}

/// Whether `b` continues `a`'s OCR line: both are OCR boxes, `b` starts to the right
/// of where `a` starts, and their vertical centres are within half a line height.
/// Words from a source's own text layer arrive in reading order, so right-to-left
/// lines join too.
fn same_ocr_line(a: &Annotation, b: &Annotation) -> bool {
    let (Some(ra), Some(rb)) = (a.region, b.region) else {
        return false;
    };
    let native =
        |x: &Annotation| x.attributes.get("text_source").map(String::as_str) == Some("native");
    let ordered = rb.x > ra.x || (native(a) && native(b));
    if a.kind != AnnotationKind::Ocr || b.kind != AnnotationKind::Ocr || !ordered {
        return false;
    }
    let centre = |r: crate::Region| r.y + r.height / 2.0;
    (centre(ra) - centre(rb)).abs() <= ra.height.max(rb.height) / 2.0
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

/// Provider name from a `provider.<name>.version` metadata key.
pub fn provider_name(key: &str) -> Option<&str> {
    key.strip_prefix("provider.")?.strip_suffix(".version")
}

/// Final path component, or empty when there is none.
pub fn basename(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl Manifest {
    pub fn build(graph: &DocumentGraph, report: &RenderReport) -> Self {
        let meta = |key: &str| graph.metadata.get(key).cloned();
        let providers = graph
            .metadata
            .iter()
            .filter_map(|(k, v)| {
                let name = provider_name(k)?;
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
                    name: basename(&s.path),
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
                let mut entities: Vec<ChunkEntity> = Vec::new();
                for a in &u.annotations {
                    if let Some(kind) = annotation_entity(a) {
                        // Dates and times list their normalized ISO 8601 value.
                        let entity = ChunkEntity {
                            kind: kind.into(),
                            value: a.attributes.get("iso").unwrap_or(&a.text).clone(),
                        };
                        if !entities.contains(&entity) {
                            entities.push(entity);
                        }
                    }
                }
                // Entities repeat text already in the chunk, so they are listed
                // separately rather than appended to it.
                let text = u.visible_text.clone().unwrap_or_else(|| {
                    annotation_text(
                        u.annotations
                            .iter()
                            .filter(|a| annotation_entity(a).is_none()),
                    )
                });
                let words = if u.kind == UnitKind::Visual {
                    u.annotations
                        .iter()
                        .filter(|a| a.kind == AnnotationKind::Ocr && annotation_entity(a).is_none())
                        .filter_map(|a| {
                            let r = a.region?.clamped();
                            Some(ChunkWord {
                                text: a.text.clone(),
                                x: r.x,
                                y: r.y,
                                width: r.width,
                                height: r.height,
                            })
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                Some(Chunk {
                    id: u.id,
                    source_id: u.source_id,
                    kind: u.kind,
                    text,
                    anchor: unit_anchor(graph, u)?,
                    pages: *report.unit_pages.get(&u.id)?,
                    providers,
                    entities,
                    words,
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
    fn ocr_words_on_one_line_join_with_spaces_in_chunk_text() {
        let word = |text: &str, x: f32, y: f32| {
            let mut a = Annotation::text(AnnotationKind::Ocr, "tesseract", text);
            a.region = Some(crate::Region {
                x,
                y,
                width: 0.1,
                height: 0.04,
            });
            a
        };
        let caption = Annotation::text(AnnotationKind::Caption, "test", "a caption");
        let annotations = [
            word("TOTAL", 0.1, 0.50),
            word("GBP", 0.4, 0.505),
            word("128.40", 0.6, 0.498),
            word("Paid:", 0.1, 0.60),
            word("VISA", 0.3, 0.60),
            caption,
            word("x", 0.2, 0.70),
            word("y", 0.1, 0.70),
        ];
        assert_eq!(
            annotation_text(annotations.iter()),
            "TOTAL GBP 128.40\nPaid: VISA\na caption\nx\ny"
        );
    }

    #[test]
    fn native_text_words_join_in_reading_order_even_right_to_left() {
        let word = |text: &str, x: f32| {
            let mut a = Annotation::text(AnnotationKind::Ocr, "pdftotext", text);
            a.region = Some(crate::Region {
                x,
                y: 0.2,
                width: 0.1,
                height: 0.04,
            });
            a.attributes.insert("text_source".into(), "native".into());
            a
        };
        let annotations = [word("שלום", 0.6), word("עולם", 0.4)];
        assert_eq!(annotation_text(annotations.iter()), "שלום עולם");
    }

    #[test]
    fn visual_chunks_list_positioned_words_and_validate() {
        let a = source("scan.png", "aa11", 10);
        let mut visual = Unit::visual(a.id, "scan.png".into());
        let mut word = Annotation::text(AnnotationKind::Ocr, "tesseract", "Invoice");
        word.region = Some(crate::Region {
            x: 0.1,
            y: 0.2,
            width: 0.3,
            height: 0.05,
        });
        let mut entity = word.clone();
        entity.attributes.insert("entity".into(), "url".into());
        let unplaced = Annotation::text(AnnotationKind::Ocr, "vision", "no box");
        let mut label = Annotation::text(AnnotationKind::Object, "yolo", "bus");
        label.region = word.region;
        visual.annotations = vec![word, entity, unplaced, label];
        let report = report(&[(visual.id, range(1, 1))]);
        let graph = DocumentGraph {
            sources: vec![a],
            units: vec![visual],
            ..Default::default()
        };
        let chunks = ChunkSet::build(&graph, &report);
        assert_eq!(
            chunks.chunks[0].words,
            vec![ChunkWord {
                text: "Invoice".into(),
                x: 0.1,
                y: 0.2,
                width: 0.3,
                height: 0.05,
            }]
        );
        let value = serde_json::to_value(&chunks).unwrap();
        let errors = crate::schema::validate(&read_schema("chunks.schema.json"), &value);
        assert!(errors.is_empty(), "chunks invalid: {errors:?}");
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
            frame: None,
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
    fn entity_annotations_are_listed_once_and_kept_out_of_chunk_text() {
        let a = source("a.png", "aa11", 10);
        let mut visual = Unit::visual(a.id, "frame.png".into());
        visual.annotations.push(Annotation::text(
            AnnotationKind::Ocr,
            "tesseract",
            "github.com",
        ));
        for _ in 0..2 {
            let mut app = Annotation::text(AnnotationKind::Custom, "text-entities", "GitHub");
            app.attributes.insert("entity".into(), "app".into());
            visual.annotations.push(app);
        }
        let report = report(&[(visual.id, range(1, 1))]);
        let graph = DocumentGraph {
            sources: vec![a],
            units: vec![visual],
            ..Default::default()
        };
        let chunks = ChunkSet::build(&graph, &report);
        let chunk = &chunks.chunks[0];
        assert_eq!(chunk.text, "github.com");
        assert_eq!(
            chunk.entities,
            vec![ChunkEntity {
                kind: "app".into(),
                value: "GitHub".into()
            }]
        );
        let value = serde_json::to_value(&chunks).unwrap();
        assert_eq!(value["chunks"][0]["entities"][0]["kind"], json!("app"));
        let errors = crate::schema::validate(&read_schema("chunks.schema.json"), &value);
        assert!(errors.is_empty(), "chunks invalid: {errors:?}");

        let mut bad = value.clone();
        bad["chunks"][0]["entities"][0]
            .as_object_mut()
            .unwrap()
            .remove("value");
        assert!(!crate::schema::validate(&read_schema("chunks.schema.json"), &bad).is_empty());
    }

    #[test]
    fn chunks_without_entities_omit_the_field() {
        let (_, chunks) = valid_pair();
        let value = serde_json::to_value(&chunks).unwrap();
        assert!(value["chunks"][0].get("entities").is_none());
        assert!(value["chunks"][0].get("words").is_none());
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
    fn frame_anchor_validates_and_negative_frame_is_rejected() {
        let a = source("a.txt", "aa11", 10);
        let mut visual = Unit::visual(a.id, "scan.tif".into());
        visual.anchor = Some(
            serde_json::from_value(json!({
                "kind":"region","x":0.0,"y":0.0,"width":1.0,"height":1.0,"frame":1
            }))
            .unwrap(),
        );
        let report = report(&[(visual.id, range(1, 1))]);
        let graph = DocumentGraph {
            sources: vec![a],
            units: vec![visual],
            ..Default::default()
        };
        let manifest = serde_json::to_value(Manifest::build(&graph, &report)).unwrap();
        let chunks = serde_json::to_value(ChunkSet::build(&graph, &report)).unwrap();
        let (ms, cs) = (
            read_schema("manifest.schema.json"),
            read_schema("chunks.schema.json"),
        );
        let mut manifest_bad = manifest.clone();
        let mut chunks_bad = chunks.clone();
        assert_eq!(manifest["units"][0]["anchor"]["frame"], json!(1));
        assert_eq!(chunks["chunks"][0]["anchor"]["frame"], json!(1));
        assert!(crate::schema::validate(&ms, &manifest).is_empty());
        assert!(crate::schema::validate(&cs, &chunks).is_empty());
        manifest_bad["units"][0]["anchor"]["frame"] = json!(-1);
        chunks_bad["chunks"][0]["anchor"]["frame"] = json!(-1);
        assert!(
            !crate::schema::validate(&ms, &manifest_bad).is_empty(),
            "manifest schema must reject frame -1"
        );
        assert!(
            !crate::schema::validate(&cs, &chunks_bad).is_empty(),
            "chunks schema must reject frame -1"
        );
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
