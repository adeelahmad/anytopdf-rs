use crate::captions::parse_cues;
use anyhow::{Context, Result};
use anytopdf_core::*;
use std::fs;

pub struct SubtitleImporter;

impl Plugin for SubtitleImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "subtitle".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: vec!["srt".into(), "vtt".into()],
            mime_types: vec!["text/vtt".into(), "application/x-subrip".into()],
            priority: 60,
        }
    }
}

impl Importer for SubtitleImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        let ext = source
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if self.descriptor().extensions.iter().any(|x| x == &ext) {
            ProbeScore::MIME
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, _ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        let raw = fs::read_to_string(&source.path)
            .with_context(|| format!("read {}", source.path.display()))?;
        let cues = parse_cues(&raw, "subtitle")?;
        let cleaned = cues
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        let mut unit = Unit::text(source.id, cleaned.clone());
        unit.annotations.extend(cues.into_iter().map(|c| {
            let mut annotation = Annotation::text(AnnotationKind::Caption, c.provider, c.text);
            annotation.time_range = Some(TimeRange {
                start_seconds: c.start,
                end_seconds: c.end,
            });
            annotation
        }));
        Ok(ImportOutcome {
            source,
            units: vec![unit],
            warnings: vec![],
        })
    }
}
