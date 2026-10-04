use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

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
    #[serde(default)]
    pub annotations: Vec<Annotation>,
    #[serde(default)]
    pub metadata: Metadata,
}

impl Unit {
    pub fn visual(source_id: Uuid, path: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4(),
            source_id,
            kind: UnitKind::Visual,
            visual_path: Some(path),
            visible_text: None,
            time_range: None,
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
            if source.sha256.is_none() {
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
}
