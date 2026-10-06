//! Cross-file search index: one SQLite database (bundled, FTS5) that records the
//! sources, units and annotations of every indexed PDF so they can be searched
//! together. The index is local to the machine; nothing in it is written into a PDF.
//!
//! Each unit becomes one `chunk` entry (its searchable text, the same text as the
//! embedded `anytopdf-chunks.json`), and each annotation becomes its own entry with
//! kind, provider, confidence, region, time range and attributes. PDFs indexed from
//! their embedded chunks (`index add`) only have chunk entries.

mod query;
mod store;

pub use query::fts_query;
pub use store::{
    DocumentSummary, Hit, HitRegion, HitSource, Index, Neighbour, PageSpan, SearchQuery, TimeSpan,
};

use anytopdf_core::{Anchor, ChunkSet, DocumentGraph, Manifest, RenderReport, Unit};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Kind of the per-unit text entry; annotation entries use the annotation kind.
pub const CHUNK_KIND: &str = "chunk";

/// Environment variable naming the index database.
pub const INDEX_ENV: &str = "ANYTOPDF_INDEX";

/// How a document entered the index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Recorded by `convert --index` from the full document graph.
    Convert,
    /// Read back from a PDF's embedded (or sidecar) chunks.
    Pdf,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Convert => "convert",
            Origin::Pdf => "pdf",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Source {
    pub id: String,
    pub name: String,
    pub path: Option<String>,
    pub media_type: Option<String>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Entry {
    pub source_id: String,
    pub unit_id: String,
    pub unit_kind: String,
    pub kind: String,
    pub text: String,
    pub provider: Option<String>,
    pub confidence: Option<f64>,
    /// Normalized x, y, width, height.
    pub region: Option<[f64; 4]>,
    pub frame: Option<u32>,
    /// Start and end, in seconds.
    pub time: Option<(f64, f64)>,
    pub pages: Option<(usize, usize)>,
    pub attributes: BTreeMap<String, String>,
}

/// Everything recorded for one PDF.
#[derive(Debug, Clone)]
pub struct Document {
    pub pdf: PathBuf,
    pub pdf_sha256: String,
    pub origin: Origin,
    pub profile: Option<String>,
    pub collection: Option<String>,
    pub sources: Vec<Source>,
    pub entries: Vec<Entry>,
}

fn unit_kind(unit: &Unit) -> String {
    serde_json::to_value(unit.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn anchor_frame(anchor: Option<&Anchor>) -> Option<u32> {
    match anchor {
        Some(Anchor::Region { frame, .. }) => *frame,
        _ => None,
    }
}

fn anchor_time(anchor: &Anchor) -> Option<(f64, f64)> {
    match anchor {
        Anchor::TimeSpan {
            start_seconds,
            end_seconds,
        } => Some((*start_seconds, *end_seconds)),
        _ => None,
    }
}

impl Document {
    /// Index records for a rendered graph: one chunk entry per unit plus one entry
    /// per annotation. `graph` must be the graph the PDF was rendered from (already
    /// filtered by the output profile), so the index holds no more than the PDF.
    pub fn from_graph(
        pdf: &Path,
        pdf_bytes: &[u8],
        graph: &DocumentGraph,
        report: &RenderReport,
    ) -> Self {
        let sources = graph
            .sources
            .iter()
            .map(|s| Source {
                id: s.id.to_string(),
                name: anytopdf_core::basename(&s.path),
                path: Some(s.path.display().to_string()).filter(|p| !p.is_empty()),
                media_type: s.detected_type.clone(),
                sha256: s.sha256.clone(),
            })
            .collect();
        let chunks = ChunkSet::build(graph, report);
        let mut entries = Vec::new();
        for unit in &graph.units {
            let pages = report.unit_pages.get(&unit.id).map(|p| (p.first, p.last));
            let base = Entry {
                source_id: unit.source_id.to_string(),
                unit_id: unit.id.to_string(),
                unit_kind: unit_kind(unit),
                frame: anchor_frame(unit.anchor.as_ref()),
                time: unit.time_range.map(|t| (t.start_seconds, t.end_seconds)),
                pages,
                ..Entry::default()
            };
            let chunk = chunks.chunks.iter().find(|c| c.id == unit.id);
            let text = chunk.map_or_else(
                || unit.visible_text.clone().unwrap_or_default(),
                |c| c.text.clone(),
            );
            if !text.trim().is_empty() {
                entries.push(Entry {
                    kind: CHUNK_KIND.into(),
                    text,
                    time: base
                        .time
                        .or_else(|| chunk.and_then(|c| anchor_time(&c.anchor))),
                    ..base.clone()
                });
            }
            for a in &unit.annotations {
                if a.text.trim().is_empty() {
                    continue;
                }
                entries.push(Entry {
                    kind: serde_json::to_value(&a.kind)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default(),
                    text: a.text.clone(),
                    provider: Some(a.provider.clone()),
                    confidence: a.confidence.map(f64::from),
                    region: a.region.map(|r| {
                        [
                            f64::from(r.x),
                            f64::from(r.y),
                            f64::from(r.width),
                            f64::from(r.height),
                        ]
                    }),
                    time: a
                        .time_range
                        .map(|t| (t.start_seconds, t.end_seconds))
                        .or(base.time),
                    attributes: a.attributes.clone(),
                    ..base.clone()
                });
            }
        }
        Self {
            pdf: pdf.to_path_buf(),
            pdf_sha256: anytopdf_core::sha256_hex(pdf_bytes),
            origin: Origin::Convert,
            profile: graph.metadata.get("anytopdf.profile").cloned(),
            collection: None,
            sources,
            entries,
        }
    }

    /// Index records read back from a PDF's embedded manifest and chunks: one chunk
    /// entry per unit. Annotation-level detail is not embedded in PDFs.
    pub fn from_chunks(
        pdf: &Path,
        pdf_bytes: &[u8],
        manifest: &Manifest,
        chunks: &ChunkSet,
    ) -> Self {
        let sources = manifest
            .sources
            .iter()
            .map(|s| Source {
                id: s.id.to_string(),
                name: s.name.clone(),
                path: s.path.clone(),
                media_type: s.media_type.clone(),
                sha256: Some(s.sha256.clone()).filter(|d| !d.is_empty()),
            })
            .collect();
        let entries = chunks
            .chunks
            .iter()
            .filter(|c| !c.text.trim().is_empty())
            .map(|c| Entry {
                source_id: c.source_id.to_string(),
                unit_id: c.id.to_string(),
                unit_kind: serde_json::to_value(c.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                kind: CHUNK_KIND.into(),
                text: c.text.clone(),
                provider: (!c.providers.is_empty()).then(|| c.providers.join(",")),
                frame: anchor_frame(Some(&c.anchor)),
                time: anchor_time(&c.anchor),
                pages: Some((c.pages.first, c.pages.last)),
                ..Entry::default()
            })
            .collect();
        Self {
            pdf: pdf.to_path_buf(),
            pdf_sha256: anytopdf_core::sha256_hex(pdf_bytes),
            origin: Origin::Pdf,
            profile: Some(manifest.profile.clone()).filter(|p| !p.is_empty()),
            collection: None,
            sources,
            entries,
        }
    }
}

/// The index used when none is named: `ANYTOPDF_INDEX`, else `index.sqlite` in the
/// per-user data directory (`%LOCALAPPDATA%\anytopdf` on Windows,
/// `~/Library/Application Support/anytopdf` on macOS, `$XDG_DATA_HOME/anytopdf` or
/// `~/.local/share/anytopdf` elsewhere).
pub fn default_index_path(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |key: &str| env(key).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(path) = set(INDEX_ENV) {
        return Some(path);
    }
    let dir = if cfg!(windows) {
        set("LOCALAPPDATA")?
    } else if cfg!(target_os = "macos") {
        set("HOME")?.join("Library/Application Support")
    } else {
        set("XDG_DATA_HOME").or_else(|| Some(set("HOME")?.join(".local/share")))?
    };
    Some(dir.join("anytopdf").join("index.sqlite"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_core::{
        Annotation, AnnotationKind, PageRange, Region, SourceRecord, TimeRange, Unit,
    };

    pub(crate) fn sample_graph() -> (DocumentGraph, RenderReport) {
        let mut source = SourceRecord::new("/videos/meeting.mp4".into());
        source.sha256 = Some("ab".repeat(32));
        source.detected_type = Some("video/mp4".into());
        let mut frame = Unit::visual(source.id, "/work/frame-1.jpg".into());
        frame.time_range = Some(TimeRange::point(725.0));
        let mut face = Annotation::text(AnnotationKind::Face, "faces", "Alice");
        face.region = Some(Region {
            x: 0.1,
            y: 0.2,
            width: 0.3,
            height: 0.4,
        });
        face.confidence = Some(0.9);
        face.attributes.insert("person".into(), "Alice".into());
        let ocr = Annotation::text(AnnotationKind::Ocr, "tesseract", "Quarterly budget review");
        frame.annotations = vec![face, ocr];
        let notes = Unit::text(source.id, "Minutes: the budget was approved.".into());
        let report = RenderReport {
            pages: 2,
            warnings: Vec::new(),
            unit_pages: [
                (frame.id, PageRange { first: 1, last: 1 }),
                (notes.id, PageRange { first: 2, last: 2 }),
            ]
            .into_iter()
            .collect(),
        };
        let mut graph = DocumentGraph {
            sources: vec![source],
            units: vec![frame, notes],
            metadata: Default::default(),
        };
        graph
            .metadata
            .insert("anytopdf.profile".into(), "archive".into());
        (graph, report)
    }

    #[test]
    fn graph_documents_hold_one_chunk_per_unit_and_one_entry_per_annotation() {
        let (graph, report) = sample_graph();
        let doc = Document::from_graph(Path::new("/out/m.pdf"), b"%PDF-", &graph, &report);
        let kinds: Vec<&str> = doc.entries.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["chunk", "face", "ocr", "chunk"]);
        let face = &doc.entries[1];
        assert_eq!(face.pages, Some((1, 1)));
        assert_eq!(face.time, Some((725.0, 725.0)));
        assert_eq!(face.attributes["person"], "Alice");
        assert!((face.region.unwrap()[2] - 0.3).abs() < 1e-6);
        assert_eq!(doc.sources[0].name, "meeting.mp4");
        assert_eq!(doc.profile.as_deref(), Some("archive"));
    }

    #[test]
    fn chunk_documents_keep_pages_and_time_anchors() {
        let (graph, report) = sample_graph();
        let manifest = Manifest::build(&graph, &report);
        let chunks = ChunkSet::build(&graph, &report);
        let doc = Document::from_chunks(Path::new("/out/m.pdf"), b"%PDF-", &manifest, &chunks);
        assert_eq!(doc.origin, Origin::Pdf);
        assert_eq!(doc.entries.len(), 2);
        assert!(doc.entries.iter().all(|e| e.kind == CHUNK_KIND));
        assert_eq!(doc.entries[0].time, Some((725.0, 725.0)));
        assert_eq!(doc.entries[1].pages, Some((2, 2)));
    }

    #[test]
    fn default_index_prefers_the_environment_variable() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert_eq!(
            default_index_path(env(&[("ANYTOPDF_INDEX", "/tmp/i.db"), ("HOME", "/h")])),
            Some(PathBuf::from("/tmp/i.db"))
        );
        let fallback = default_index_path(env(&[
            ("HOME", "/h"),
            ("LOCALAPPDATA", "C:/l"),
            ("XDG_DATA_HOME", "/x"),
        ]))
        .unwrap();
        assert!(fallback.ends_with("anytopdf/index.sqlite"));
        assert_eq!(default_index_path(env(&[])), None);
    }
}
