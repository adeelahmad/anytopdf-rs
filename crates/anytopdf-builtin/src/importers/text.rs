use anyhow::{Context, Result};
use anytopdf_core::*;
use std::fs;
use std::io::Read;

pub struct TextImporter;

const TEXT_SNIFF: ProbeScore = ProbeScore(100);
const SNIFF_BYTES: usize = 8 * 1024;

fn looks_like_text(prefix: &[u8]) -> bool {
    if prefix.contains(&0) {
        return false;
    }
    if std::str::from_utf8(prefix).is_ok() {
        return true;
    }
    let texty = prefix
        .iter()
        .filter(|b| matches!(**b, b'\t' | b'\n' | b'\r') || (**b >= 0x20 && **b != 0x7f))
        .count();
    texty * 100 >= prefix.len() * 95
}

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
            let mut prefix = Vec::new();
            let read = fs::File::open(&source.path)
                .and_then(|f| f.take(SNIFF_BYTES as u64).read_to_end(&mut prefix));
            if read.is_ok() && looks_like_text(&prefix) {
                TEXT_SNIFF
            } else {
                ProbeScore::NONE
            }
        }
    }

    fn import(&self, _ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        let bytes =
            fs::read(&source.path).with_context(|| format!("read {}", source.path.display()))?;
        let (text, warnings) = match String::from_utf8(bytes) {
            Ok(text) => (text, vec![]),
            Err(e) => {
                let name = source
                    .path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                let warning = Diagnostic::new(
                    DiagnosticCode::LossyDecode,
                    format!("{name} is not valid UTF-8; invalid bytes were replaced"),
                );
                (
                    String::from_utf8_lossy(e.as_bytes()).into_owned(),
                    vec![warning.to_string()],
                )
            }
        };
        Ok(ImportOutcome {
            units: vec![Unit::text(source.id, text)],
            source,
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinOptions, register_builtins};
    use std::path::Path;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> SourceRecord {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        SourceRecord::new(path)
    }

    fn import(dir: &Path, source: SourceRecord) -> Result<ImportOutcome> {
        let ctx = JobContext {
            workspace: dir.into(),
            quiet: true,
        };
        TextImporter.import(&ctx, source)
    }

    #[test]
    fn latin1_text_is_decoded_lossily_with_coded_warning() {
        let dir = tempfile::tempdir().unwrap();
        let source = write(dir.path(), "latin1.txt", b"caf\xe9 r\xe9sum\xe9\n");
        let outcome = import(dir.path(), source);
        assert!(outcome.is_ok(), "{:?}", outcome.as_ref().err());
        let outcome = outcome.unwrap();
        let text = outcome.units[0].visible_text.as_deref().unwrap();
        assert!(text.contains("caf\u{FFFD}"), "{text:?}");
        assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
        let d = Diagnostic::from_wire(&outcome.warnings[0]);
        assert_eq!(d.code, DiagnosticCode::LossyDecode);
        assert!(d.message.contains("latin1.txt"), "{}", d.message);
    }

    #[test]
    fn csv_json_log_and_rust_sources_are_accepted_by_sniffing() {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in [
            ("data.csv", "a,b\n1,2\n"),
            ("doc.json", "{\"k\":1}\n"),
            ("app.log", "INFO start\n"),
            ("main.rs", "fn main() {}\n"),
        ] {
            let source = write(dir.path(), name, content.as_bytes());
            let score = TextImporter.probe(&source);
            assert!(score > ProbeScore::NONE, "{name}: {score:?}");
            assert!(score < ProbeScore::EXTENSION, "{name}: {score:?}");
            let outcome = import(dir.path(), source);
            assert!(outcome.is_ok(), "{name}: {:?}", outcome.as_ref().err());
            let outcome = outcome.unwrap();
            assert_eq!(outcome.units.len(), 1, "{name}");
            assert_eq!(outcome.units[0].visible_text.as_deref(), Some(content));
            assert!(
                outcome.warnings.is_empty(),
                "{name}: {:?}",
                outcome.warnings
            );
        }
    }

    #[test]
    fn binary_content_is_not_sniffed_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let blob = write(dir.path(), "blob.bin", &[0u8; 16]);
        assert_eq!(TextImporter.probe(&blob), ProbeScore::NONE);
        let png_path = dir.path().join("image.dat");
        image::RgbImage::new(4, 3)
            .save_with_format(&png_path, image::ImageFormat::Png)
            .unwrap();
        let png = SourceRecord::new(png_path);
        assert_eq!(TextImporter.probe(&png), ProbeScore::NONE);
    }

    #[test]
    fn subtitle_and_media_importers_still_outrank_sniffed_text() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::default();
        register_builtins(&mut registry, BuiltinOptions::default());
        let srt = write(
            dir.path(),
            "cues.srt",
            b"1\n00:00:01,000 --> 00:00:02,000\nhello\n\n",
        );
        let mp4 = write(dir.path(), "movie.mp4", b"not actually a video");
        assert_eq!(
            registry.importer_for(&srt).unwrap().descriptor().name,
            "subtitle"
        );
        assert_eq!(
            registry.importer_for(&mp4).unwrap().descriptor().name,
            "ffmpeg-video"
        );
    }
}
