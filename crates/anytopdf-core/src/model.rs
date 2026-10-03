use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

pub type Metadata = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceRecord {
    pub id: Uuid,
    pub path: PathBuf,
    pub detected_type: Option<String>,
    #[serde(default)]
    pub metadata: Metadata,
}

impl SourceRecord {
    pub fn new(path: PathBuf) -> Self {
        Self {
            id: Uuid::new_v4(),
            path,
            detected_type: None,
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
