use anyhow::{Context, Result};
use anytopdf_core::*;
use std::fs;

pub struct TextImporter;

impl Plugin for TextImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "text".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: vec!["txt".into(), "md".into()],
            mime_types: vec!["text/plain".into(), "text/markdown".into()],
            priority: 30,
        }
    }
}

impl Importer for TextImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        let ext = source
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if self.descriptor().extensions.iter().any(|x| x == &ext) {
            ProbeScore::EXTENSION
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, _ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        let text = fs::read_to_string(&source.path)
            .with_context(|| format!("read {}", source.path.display()))?;
        Ok(ImportOutcome {
            units: vec![Unit::text(source.id, text)],
            source,
            warnings: vec![],
        })
    }
}
