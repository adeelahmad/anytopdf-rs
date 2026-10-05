use anyhow::{Context, Result};
use anytopdf_core::*;
use serde_json::Value;
use std::process::Command;

#[derive(Default)]
pub struct MetadataEnricher;

impl Plugin for MetadataEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "metadata".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "source-enricher".into(),
            extensions: vec![],
            mime_types: vec!["*/*".into()],
            priority: 0,
        }
    }
}

impl SourceEnricher for MetadataEnricher {
    fn supports(&self, _source: &SourceRecord) -> bool {
        true
    }

    fn enrich_source(&self, _ctx: &JobContext, source: &mut SourceRecord) -> Result<Vec<String>> {
        let mut warnings = Vec::new();

        if let Ok(meta) = source.path.metadata() {
            source
                .metadata
                .insert("fs.size-bytes".into(), meta.len().to_string());
        }
        source
            .metadata
            .insert("source.path".into(), source.path.display().to_string());
        source.metadata.insert(
            "source.filename".into(),
            anytopdf_core::basename(&source.path),
        );

        if let Ok(exiftool) = which::which("exiftool") {
            let out = Command::new(exiftool)
                .args(["-j", "-G1", "-a", "-s", "-charset", "filename=UTF8"])
                .arg(&source.path)
                .bounded_output(std::time::Duration::from_secs(30))
                .context("run exiftool")?;
            if out.status.success() {
                if let Ok(json) = serde_json::from_slice::<Value>(&out.stdout) {
                    if let Some(obj) = json
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|v| v.as_object())
                    {
                        flatten("exiftool", obj, &mut source.metadata);
                    }
                }
            } else {
                warnings.push(
                    Diagnostic::new(
                        DiagnosticCode::ProviderFailed,
                        format!(
                            "exiftool failed for {}: {}",
                            source.path.display(),
                            String::from_utf8_lossy(&out.stderr)
                        ),
                    )
                    .to_string(),
                );
            }
        } else {
            warnings.push(missing_provider_notice(
                "exiftool",
                "rich EXIF/XMP metadata disabled",
            ));
        }

        if source
            .detected_type
            .as_deref()
            .is_some_and(|m| m.starts_with("video/") || m.starts_with("audio/"))
        {
            if let Ok(ffprobe) = which::which("ffprobe") {
                let out = Command::new(ffprobe)
                    .args([
                        "-v",
                        "error",
                        "-protocol_whitelist",
                        crate::FFMPEG_PROTOCOLS,
                        "-show_format",
                        "-show_streams",
                        "-of",
                        "json",
                    ])
                    .arg(&source.path)
                    .bounded_output(std::time::Duration::from_secs(30))
                    .context("run ffprobe")?;
                if out.status.success() {
                    if let Ok(value) = serde_json::from_slice::<Value>(&out.stdout) {
                        flatten_value("ffprobe", &value, &mut source.metadata);
                        // MP4/Ogg magic identifies a container; inspect streams to
                        // avoid sending audio-only containers to the video importer.
                        let streams = value["streams"].as_array();
                        let has_audio = streams.is_some_and(|streams| {
                            streams.iter().any(|s| s["codec_type"] == "audio")
                        });
                        let has_video = streams.is_some_and(|streams| {
                            streams.iter().any(|s| {
                                s["codec_type"] == "video" && s["disposition"]["attached_pic"] != 1
                            })
                        });
                        if has_audio && !has_video {
                            if let Some(mime) = source.detected_type.as_mut() {
                                if let Some(subtype) = mime.strip_prefix("video/") {
                                    *mime = format!("audio/{subtype}");
                                }
                            }
                        }
                    }
                } else {
                    warnings.push(
                        Diagnostic::new(
                            DiagnosticCode::ProviderFailed,
                            format!(
                                "ffprobe failed for {}: {}",
                                source.path.display(),
                                String::from_utf8_lossy(&out.stderr)
                            ),
                        )
                        .to_string(),
                    );
                }
            }
        }

        Ok(warnings)
    }
}

fn flatten(prefix: &str, obj: &serde_json::Map<String, Value>, out: &mut Metadata) {
    for (k, v) in obj {
        flatten_value(&format!("{prefix}.{k}"), v, out);
    }
}

fn flatten_value(prefix: &str, value: &Value, out: &mut Metadata) {
    match value {
        Value::Object(obj) => flatten(prefix, obj, out),
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                flatten_value(&format!("{prefix}[{i}]"), item, out);
            }
        }
        Value::Null => {}
        Value::String(s) => {
            out.insert(prefix.into(), s.clone());
        }
        other => {
            out.insert(prefix.into(), other.to_string());
        }
    }
}

fn missing_provider_notice(provider: &str, consequence: &str) -> String {
    Diagnostic::new(
        DiagnosticCode::ProviderMissing,
        format!("{provider} unavailable; {consequence}"),
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_exiftool_notice_is_informational() {
        let d = Diagnostic::from_wire(&missing_provider_notice(
            "exiftool",
            "rich EXIF/XMP metadata disabled",
        ));
        assert_eq!(d.code, DiagnosticCode::ProviderMissing);
        assert_eq!(d.severity, Severity::Info);
        assert!(d.message.contains("exiftool"), "{}", d.message);
    }

    #[test]
    fn source_filename_is_the_base_name() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("dir")).unwrap();
        let path = dir.path().join("dir").join("report.txt");
        std::fs::write(&path, b"hello\n").unwrap();
        let mut source = SourceRecord::new(path);
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        MetadataEnricher.enrich_source(&ctx, &mut source).unwrap();
        assert_eq!(source.metadata["source.filename"], "report.txt");
        assert!(
            source.metadata["source.path"].ends_with("report.txt"),
            "{:?}",
            source.metadata["source.path"]
        );
    }
}
