use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
pub use uuid::Uuid;

pub type Metadata = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    pub id: Uuid,
    pub path: PathBuf,
    pub detected_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default)]
    pub metadata: Metadata,
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn derived_uuid(domain: &str, parts: &[&str]) -> Uuid {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    for part in parts {
        hasher.update([0]);
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}

pub fn content_source_id(sha256_hex: &str, occurrence: usize) -> Uuid {
    derived_uuid("anytopdf/source/v1", &[sha256_hex, &occurrence.to_string()])
}

pub fn content_unit_id(source_id: Uuid, position: usize) -> Uuid {
    derived_uuid(
        "anytopdf/unit/v1",
        &[&source_id.to_string(), &position.to_string()],
    )
}

impl SourceRecord {
    pub fn new(path: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4(),
            path,
            detected_type: None,
            sha256: None,
            size: None,
            metadata: Metadata::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UnitKind {
    Visual,
    Text,
    Audio,
    ExistingPdf,
    Generic,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Region {
    /// Normalized top-left X.
    pub x: f32,
    /// Normalized top-left Y.
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Region {
    pub fn clamped(self) -> Self {
        let finite = |value: f32| if value.is_finite() { value } else { 0.0 };
        let x = finite(self.x).clamp(0.0, 1.0);
        let y = finite(self.y).clamp(0.0, 1.0);
        let width = finite(self.width).max(0.0).min(1.0 - x);
        let height = finite(self.height).max(0.0).min(1.0 - y);
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct TimeRange {
    pub start_seconds: f64,
    pub end_seconds: f64,
}

impl TimeRange {
    pub fn point(seconds: f64) -> Self {
        Self {
            start_seconds: seconds,
            end_seconds: seconds,
        }
    }

    pub fn contains(&self, seconds: f64, padding: f64) -> bool {
        seconds >= self.start_seconds - padding && seconds <= self.end_seconds + padding
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnnotationKind {
    Ocr,
    Caption,
    Transcript,
    Metadata,
    Object,
    Face,
    Scene,
    Timestamp,
    Location,
    Barcode,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Annotation {
    pub kind: AnnotationKind,
    pub text: String,
    pub provider: String,
    pub confidence: Option<f32>,
    pub region: Option<Region>,
    pub time_range: Option<TimeRange>,
    #[serde(default)]
    pub attributes: Metadata,
}

impl Annotation {
    pub fn text(
        kind: AnnotationKind,
        provider: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            text: text.into(),
            provider: provider.into(),
            confidence: None,
            region: None,
            time_range: None,
            attributes: Metadata::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Anchor {
    TimeSpan {
        start_seconds: f64,
        end_seconds: f64,
    },
    Region {
        x: f32,
        y: f32,
        width: f32,
        height: f32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        frame: Option<u32>,
    },
    ByteRange {
        start: u64,
        end: u64,
    },
}

/// Unit metadata key controlling page layout. With the value
/// [`LAYOUT_FLOW_CONTINUOUS`], a text unit continues on the page where the
/// previous text unit of the same source ended instead of starting a new page.
/// Record-oriented importers (JSON Lines, CSV rows, chat messages) set it so
/// each record stays its own chunk without costing a page.
pub const LAYOUT_FLOW_KEY: &str = "layout.flow";
pub const LAYOUT_FLOW_CONTINUOUS: &str = "continuous";

/// Unit metadata key naming the container member (email attachment, archive
/// member) a unit was imported from, as readers see it.
pub const MEMBER_KEY: &str = "container.member";
/// Unit metadata key holding the detected media type of that member, such as
/// `image/jpeg` for a photo attached to an email. Enrichers that pick inputs
/// by media type match it instead of the container's type; see
/// [`DocumentGraph::unit_source`].
pub const MEMBER_TYPE_KEY: &str = "container.member-type";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Unit {
    #[serde(default = "Uuid::new_v4")]
    pub id: Uuid,
    #[serde(default)]
    pub source_id: Uuid,
    pub kind: UnitKind,
    /// A normalized visual representation. Importers may point at the original
    /// image or at a derived keyframe/page image in the job workspace.
    pub visual_path: Option<PathBuf>,
    /// Text that should be visibly laid out as document content.
    pub visible_text: Option<String>,
    pub time_range: Option<TimeRange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    #[serde(default)]
    pub metadata: Metadata,
}

impl Unit {
    pub fn default_anchor(&self, source: &SourceRecord) -> Anchor {
        let annotation_times = self
            .annotations
            .iter()
            .filter_map(|a| a.time_range.as_ref())
            .copied()
            .reduce(|a, b| TimeRange {
                start_seconds: a.start_seconds.min(b.start_seconds),
                end_seconds: a.end_seconds.max(b.end_seconds),
            });
        if let Some(t) = self.time_range.or(annotation_times) {
            return Anchor::TimeSpan {
                start_seconds: t.start_seconds,
                end_seconds: t.end_seconds,
            };
        }
        if self.kind == UnitKind::Visual {
            return Anchor::Region {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                frame: None,
            };
        }
        Anchor::ByteRange {
            start: 0,
            end: source.size.unwrap_or(0),
        }
    }

    /// Whether this unit may share a page with `previous`: both are text-only
    /// units of the same source and this one asks for continuous flow.
    pub fn flows_after(&self, previous: &Unit) -> bool {
        let text_only = |u: &Unit| u.visual_path.is_none() && u.visible_text.is_some();
        text_only(self)
            && text_only(previous)
            && self.source_id == previous.source_id
            && self
                .metadata
                .get(LAYOUT_FLOW_KEY)
                .is_some_and(|v| v == LAYOUT_FLOW_CONTINUOUS)
    }

    pub fn visual(source_id: Uuid, path: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4(),
            source_id,
            kind: UnitKind::Visual,
            visual_path: Some(path),
            visible_text: None,
            time_range: None,
            anchor: None,
            annotations: Vec::new(),
            metadata: Metadata::new(),
        }
    }

    pub fn text(source_id: Uuid, text: String) -> Self {
        Self {
            id: Uuid::new_v4(),
            source_id,
            kind: UnitKind::Text,
            visual_path: None,
            visible_text: Some(text),
            time_range: None,
            anchor: None,
            annotations: Vec::new(),
            metadata: Metadata::new(),
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct DocumentGraph {
    pub sources: Vec<SourceRecord>,
    pub units: Vec<Unit>,
    #[serde(default)]
    pub metadata: Metadata,
}

impl DocumentGraph {
    pub fn validate(&self) -> anyhow::Result<()> {
        use std::collections::HashSet;
        let mut sources = HashSet::new();
        let mut units = HashSet::new();
        for source in &self.sources {
            anyhow::ensure!(
                !source.id.is_nil() && sources.insert(source.id),
                "duplicate or nil source ID"
            );
        }
        for unit in &self.units {
            anyhow::ensure!(
                !unit.id.is_nil() && units.insert(unit.id),
                "duplicate or nil unit ID"
            );
            anyhow::ensure!(
                sources.contains(&unit.source_id),
                "unit references unknown source"
            );
            for time in unit.time_range.iter().chain(
                unit.annotations
                    .iter()
                    .filter_map(|a| a.time_range.as_ref()),
            ) {
                anyhow::ensure!(
                    time.start_seconds.is_finite()
                        && time.end_seconds.is_finite()
                        && time.start_seconds >= 0.0
                        && time.end_seconds >= time.start_seconds,
                    "invalid annotation time range"
                );
            }
            match &unit.anchor {
                Some(Anchor::TimeSpan {
                    start_seconds,
                    end_seconds,
                }) => anyhow::ensure!(
                    start_seconds.is_finite()
                        && end_seconds.is_finite()
                        && *start_seconds >= 0.0
                        && end_seconds >= start_seconds,
                    "invalid anchor time span"
                ),
                Some(Anchor::Region {
                    x,
                    y,
                    width,
                    height,
                    ..
                }) => anyhow::ensure!(
                    [x, y, width, height].iter().all(|n| n.is_finite()),
                    "non-finite anchor region"
                ),
                Some(Anchor::ByteRange { start, end }) => {
                    anyhow::ensure!(end >= start, "inverted anchor byte range")
                }
                None => {}
            }
            for annotation in &unit.annotations {
                anyhow::ensure!(
                    !annotation.provider.trim().is_empty(),
                    "annotation must record its provider"
                );
                if let Some(confidence) = annotation.confidence {
                    anyhow::ensure!(
                        confidence.is_finite() && (0.0..=1.0).contains(&confidence),
                        "invalid annotation confidence"
                    );
                }
                if let Some(region) = annotation.region {
                    anyhow::ensure!(
                        [region.x, region.y, region.width, region.height]
                            .iter()
                            .all(|n| n.is_finite()),
                        "non-finite annotation region"
                    );
                }
            }
        }
        Ok(())
    }

    pub fn assign_content_ids(&mut self) -> anyhow::Result<()> {
        use anyhow::Context;
        use std::collections::HashMap;
        let mut occurrences: HashMap<String, usize> = HashMap::new();
        let mut remap: HashMap<Uuid, Uuid> = HashMap::new();
        for source in &mut self.sources {
            // A supplied digest is trusted only for sources the host cannot read itself, such as
            // virtual sources a plugin adds; a file on disk is always hashed by the host.
            if source.sha256.is_none() || source.path.is_file() {
                let bytes = std::fs::read(&source.path)
                    .with_context(|| format!("read {} for digest", source.path.display()))?;
                source.size = Some(bytes.len() as u64);
                source.sha256 = Some(sha256_hex(&bytes));
            }
            let digest = source.sha256.clone().unwrap_or_default();
            let occurrence = occurrences.entry(digest.clone()).or_insert(0);
            let id = content_source_id(&digest, *occurrence);
            *occurrence += 1;
            remap.insert(source.id, id);
            source.id = id;
        }
        let mut positions: HashMap<Uuid, usize> = HashMap::new();
        for unit in &mut self.units {
            unit.source_id = *remap.get(&unit.source_id).unwrap_or(&unit.source_id);
            let position = positions.entry(unit.source_id).or_insert(0);
            unit.id = content_unit_id(unit.source_id, *position);
            *position += 1;
        }
        Ok(())
    }

    pub fn source(&self, id: Uuid) -> Option<&SourceRecord> {
        self.sources.iter().find(|s| s.id == id)
    }

    /// The source of `unit` as an enricher that picks inputs by media type
    /// should see it. Units imported from a container member carry the
    /// member's type in [`MEMBER_TYPE_KEY`]; for those this is the container's
    /// record with the member's type and file name, so a photo attached to an
    /// email is treated as an image rather than as an email.
    pub fn unit_source(&self, unit: &Unit) -> Option<std::borrow::Cow<'_, SourceRecord>> {
        let source = self.source(unit.source_id)?;
        let Some(member_type) = unit.metadata.get(MEMBER_TYPE_KEY) else {
            return Some(std::borrow::Cow::Borrowed(source));
        };
        let mut member = source.clone();
        member.detected_type = Some(member_type.clone());
        if let Some(name) = unit
            .metadata
            .get(MEMBER_KEY)
            .and_then(|n| n.rsplit(['/', '\\']).next())
            .filter(|n| !n.is_empty())
        {
            member.path = source.path.with_file_name(name);
        }
        Some(std::borrow::Cow::Owned(member))
    }

    pub fn source_mut(&mut self, id: Uuid) -> Option<&mut SourceRecord> {
        self.sources.iter_mut().find(|s| s.id == id)
    }
}

#[derive(Debug, Clone)]
pub struct JobContext {
    pub workspace: PathBuf,
    pub quiet: bool,
}

#[derive(Debug, Clone)]
pub struct ImportOutcome {
    pub source: SourceRecord,
    pub units: Vec<Unit>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderReport {
    pub pages: usize,
    pub warnings: Vec<String>,
    #[serde(default)]
    pub unit_pages: BTreeMap<Uuid, PageRange>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRange {
    pub first: usize,
    pub last: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &[u8] = b"Identity fixture\n";
    const DIGEST: &str = "eea2ca13a1da285c9365c7dd3fdfb68eb34445313f8eb1b994991728ae3d917a";

    fn graph_for(dir: &std::path::Path, files: &[(&str, &[u8])]) -> DocumentGraph {
        let mut graph = DocumentGraph::default();
        for (name, bytes) in files {
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            let source = SourceRecord::new(path);
            graph.units.push(Unit::text(source.id, "t".into()));
            graph.sources.push(source);
        }
        graph
    }

    #[test]
    fn supplied_digest_is_replaced_for_files_and_kept_for_virtual_sources() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = graph_for(dir.path(), &[("a.txt", FIXTURE)]);
        let forged = "0".repeat(64);
        graph.sources[0].sha256 = Some(forged.clone());
        graph.sources[0].size = Some(1);
        let mut remote = SourceRecord::new(dir.path().join("not-on-disk.igl"));
        remote.sha256 = Some(forged.clone());
        graph.units.push(Unit::text(remote.id, "virtual".into()));
        graph.sources.push(remote);
        graph.assign_content_ids().unwrap();
        assert_eq!(graph.sources[0].sha256.as_deref(), Some(DIGEST));
        assert_eq!(graph.sources[0].size, Some(17));
        assert_eq!(graph.sources[0].id, content_source_id(DIGEST, 0));
        assert_eq!(graph.sources[1].sha256.as_deref(), Some(forged.as_str()));
    }

    #[test]
    fn content_ids_are_stable_for_identical_bytes() {
        let (d1, d2) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut a = graph_for(d1.path(), &[("a.txt", FIXTURE)]);
        let mut b = graph_for(d2.path(), &[("b.txt", FIXTURE)]);
        a.assign_content_ids().unwrap();
        b.assign_content_ids().unwrap();
        assert_eq!(a.sources[0].id, b.sources[0].id);
        assert_eq!(a.units[0].id, b.units[0].id);
        assert_eq!(a.sources[0].sha256.as_deref(), Some(DIGEST));
        assert_eq!(a.sources[0].size, Some(17));
        assert_eq!(a.sources[0].id, content_source_id(DIGEST, 0));
    }

    #[test]
    fn changing_one_byte_changes_only_that_source_identity() {
        let dir = tempfile::tempdir().unwrap();
        let mut flipped = FIXTURE.to_vec();
        flipped[0] ^= 1;
        let mut before = graph_for(
            dir.path(),
            &[("a.txt", FIXTURE), ("b.txt", b"other bytes\n")],
        );
        before.assign_content_ids().unwrap();
        let mut after = graph_for(dir.path(), &[("a.txt", FIXTURE), ("b.txt", &flipped)]);
        after.assign_content_ids().unwrap();
        assert_eq!(before.sources[0].id, after.sources[0].id);
        assert_eq!(before.sources[0].sha256, after.sources[0].sha256);
        assert_ne!(before.sources[1].id, after.sources[1].id);
        assert_ne!(before.sources[1].sha256, after.sources[1].sha256);
    }

    #[test]
    fn identical_content_sources_get_distinct_ids() {
        let dir = tempfile::tempdir().unwrap();
        let mut graph = graph_for(dir.path(), &[("a.txt", FIXTURE), ("b.txt", FIXTURE)]);
        graph.assign_content_ids().unwrap();
        assert_ne!(graph.sources[0].id, graph.sources[1].id);
        assert_eq!(graph.sources[0].id, content_source_id(DIGEST, 0));
        assert_eq!(graph.sources[1].id, content_source_id(DIGEST, 1));
        graph.validate().unwrap();
        for (unit, source) in graph.units.iter().zip(&graph.sources) {
            assert_eq!(unit.source_id, source.id);
        }
    }

    #[test]
    fn validation_rejects_orphaned_units_and_duplicate_ids() {
        let source = SourceRecord::new("source.txt".into());
        let unit = Unit::text(source.id, "content".into());
        let mut graph = DocumentGraph {
            units: vec![unit.clone()],
            ..Default::default()
        };
        assert!(graph.validate().is_err());
        graph.sources.push(source);
        assert!(graph.validate().is_ok());
        graph.units.push(unit);
        assert!(graph.validate().is_err());
    }

    #[test]
    fn regions_never_propagate_nan_or_escape_page() {
        let region = Region {
            x: 0.8,
            y: f32::NAN,
            width: 0.5,
            height: f32::INFINITY,
        }
        .clamped();
        assert!(region.width <= 0.2);
        assert_eq!(region.y, 0.0);
        assert_eq!(region.height, 0.0);
    }

    const FRAME_ONE: &str =
        r#"{"kind":"region","x":0.0,"y":0.0,"width":1.0,"height":1.0,"frame":1}"#;

    #[test]
    fn region_anchor_frame_round_trips() {
        let anchor: Anchor = serde_json::from_str(FRAME_ONE).unwrap();
        assert!(matches!(anchor, Anchor::Region { frame: Some(1), .. }));
        assert_eq!(serde_json::to_string(&anchor).unwrap(), FRAME_ONE);
    }

    #[test]
    fn region_anchor_without_frame_serializes_as_before() {
        let mut source = SourceRecord::new("s.bin".into());
        source.size = Some(42);
        let visual = Unit::visual(source.id, "v.png".into());
        assert_eq!(
            serde_json::to_string(&visual.default_anchor(&source)).unwrap(),
            r#"{"kind":"region","x":0.0,"y":0.0,"width":1.0,"height":1.0}"#
        );
    }

    #[test]
    fn default_anchor_matches_unit_kind() {
        let mut source = SourceRecord::new("s.bin".into());
        source.size = Some(42);
        let text = Unit::text(source.id, "t".into());
        assert_eq!(
            text.default_anchor(&source),
            Anchor::ByteRange { start: 0, end: 42 }
        );
        let visual = Unit::visual(source.id, "v.png".into());
        assert_eq!(
            visual.default_anchor(&source),
            Anchor::Region {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                frame: None,
            }
        );
        let mut timed = Unit::visual(source.id, "v.png".into());
        timed.time_range = Some(TimeRange::point(5.0));
        assert_eq!(
            timed.default_anchor(&source),
            Anchor::TimeSpan {
                start_seconds: 5.0,
                end_seconds: 5.0
            }
        );
        let mut transcript = Unit::text(source.id, "t".into());
        for (start, end) in [(1.0, 2.0), (3.0, 4.5)] {
            let mut cue = Annotation::text(AnnotationKind::Transcript, "test", "cue");
            cue.time_range = Some(TimeRange {
                start_seconds: start,
                end_seconds: end,
            });
            transcript.annotations.push(cue);
        }
        assert_eq!(
            transcript.default_anchor(&source),
            Anchor::TimeSpan {
                start_seconds: 1.0,
                end_seconds: 4.5
            }
        );
    }

    #[test]
    fn validation_rejects_inverted_or_non_finite_anchors() {
        let source = SourceRecord::new("s.bin".into());
        let check = |anchor: Anchor| {
            let mut unit = Unit::text(source.id, "t".into());
            unit.anchor = Some(anchor);
            DocumentGraph {
                sources: vec![source.clone()],
                units: vec![unit],
                ..Default::default()
            }
            .validate()
        };
        assert!(check(Anchor::ByteRange { start: 10, end: 2 }).is_err());
        assert!(
            check(Anchor::TimeSpan {
                start_seconds: -1.0,
                end_seconds: 2.0
            })
            .is_err()
        );
        assert!(
            check(Anchor::TimeSpan {
                start_seconds: 3.0,
                end_seconds: 1.0
            })
            .is_err()
        );
        assert!(
            check(Anchor::Region {
                x: f32::NAN,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                frame: None,
            })
            .is_err()
        );
        assert!(check(Anchor::ByteRange { start: 0, end: 2 }).is_ok());
        assert!(
            check(Anchor::TimeSpan {
                start_seconds: 1.0,
                end_seconds: 1.0
            })
            .is_ok()
        );
        assert!(
            check(Anchor::Region {
                x: 0.0,
                y: 0.0,
                width: 1.0,
                height: 1.0,
                frame: None,
            })
            .is_ok()
        );
    }

    #[test]
    fn render_report_without_unit_pages_still_deserializes() {
        let report = serde_json::from_str::<RenderReport>(r#"{"pages":1,"warnings":[]}"#)
            .expect("a v1 renderer report without unit_pages must parse");
        assert!(report.unit_pages.is_empty());
        assert_eq!(report.pages, 1);
    }
}
