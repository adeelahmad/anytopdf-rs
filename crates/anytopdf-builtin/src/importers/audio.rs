use anyhow::Result;
use anytopdf_core::*;

pub struct AudioImporter;

impl Plugin for AudioImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "audio".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: [
                "mp3", "m4a", "aac", "wav", "flac", "ogg", "oga", "opus", "aiff", "aif", "caf",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            mime_types: vec!["audio/*".into()],
            priority: 40,
        }
    }
}

impl Importer for AudioImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        if source
            .detected_type
            .as_deref()
            .is_some_and(|m| m.starts_with("audio/"))
        {
            return ProbeScore::MAGIC;
        }
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
        let mut unit = Unit {
            id: uuid::Uuid::new_v4(),
            source_id: source.id,
            kind: UnitKind::Audio,
            visual_path: None,
            visible_text: Some(format!(
                "Audio source: {}\n\nNo speech-to-text provider produced a transcript. \
                 Supply --transcript or install/register a transcription plugin.",
                source
                    .path
                    .file_name()
                    .and_then(|x| x.to_str())
                    .unwrap_or("audio")
            )),
            time_range: None,
            anchor: None,
            annotations: vec![],
            metadata: Metadata::new(),
        };
        unit.annotations.push(Annotation::text(
            AnnotationKind::Custom,
            "audio",
            "audio source",
        ));
        Ok(ImportOutcome {
            source,
            units: vec![unit],
            warnings: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_placeholder_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("talk.mp3");
        std::fs::write(&path, b"not really audio").unwrap();
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let outcome = AudioImporter.import(&ctx, SourceRecord::new(path)).unwrap();
        let text = outcome.units[0].visible_text.as_deref().unwrap();
        assert!(text.starts_with("Audio source: talk.mp3\n\n"), "{text:?}");
    }
}
