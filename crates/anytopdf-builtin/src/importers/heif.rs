use anyhow::{Context, Result};
use anytopdf_core::*;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// HEIC/HEIF and AVIF photos. Decoding HEVC/AV1 needs native codecs, so the
/// image is converted to PNG by the first available local tool and then
/// treated like any other raster page (OCR included).
pub struct HeifImporter;

const CONVERT_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Converter {
    /// macOS built-in.
    Sips,
    /// libheif's command-line tool.
    HeifConvert,
    /// ImageMagick 7.
    Magick,
    /// ImageMagick 6; skipped on Windows, where `convert` is a disk utility.
    Convert,
}

impl Converter {
    const ALL: [Converter; 4] = [
        Converter::Sips,
        Converter::HeifConvert,
        Converter::Magick,
        Converter::Convert,
    ];

    fn program(self) -> &'static str {
        match self {
            Converter::Sips => "sips",
            Converter::HeifConvert => "heif-convert",
            Converter::Magick => "magick",
            Converter::Convert => "convert",
        }
    }

    fn usable_here(self) -> bool {
        match self {
            Converter::Sips => cfg!(target_os = "macos"),
            Converter::Convert => !cfg!(windows),
            _ => true,
        }
    }

    fn command(self, program: &Path, input: &Path, output: &Path) -> Command {
        let mut cmd = Command::new(program);
        match self {
            Converter::Sips => {
                cmd.args(["-s", "format", "png"])
                    .arg(input)
                    .arg("--out")
                    .arg(output);
            }
            Converter::HeifConvert => {
                cmd.arg(input).arg(output);
            }
            Converter::Magick | Converter::Convert => {
                // `[0]` selects the primary image; -auto-orient applies EXIF/irot.
                let mut first = input.as_os_str().to_owned();
                first.push("[0]");
                cmd.arg(first).arg("-auto-orient").arg(output);
            }
        }
        cmd
    }
}

/// The PNG a converter produced. heif-convert numbers its outputs
/// (`out-1.png`, ...) when a file holds several top-level images; the first
/// is the primary one.
fn produced(output: &Path) -> Option<PathBuf> {
    if output.is_file() {
        return Some(output.to_path_buf());
    }
    let stem = output.file_stem()?.to_string_lossy();
    let numbered = output.with_file_name(format!("{stem}-1.png"));
    numbered.is_file().then_some(numbered)
}

fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("no output")
        .to_string()
}

impl Plugin for HeifImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "heif".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: ["heic", "heif", "hif", "avif"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            mime_types: ["image/heif", "image/heic", "image/avif"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            priority: 50,
        }
    }
}

impl Importer for HeifImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        if source
            .detected_type
            .as_deref()
            .is_some_and(|m| self.descriptor().mime_types.iter().any(|x| x == m))
        {
            return ProbeScore::MAGIC;
        }
        let ext = source
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if self.descriptor().extensions.contains(&ext) {
            ProbeScore::EXTENSION
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        let output = ctx.workspace.join(format!("heif-{}.png", source.id));
        let mut failures = Vec::new();
        for converter in Converter::ALL.into_iter().filter(|c| c.usable_here()) {
            let Ok(program) = which::which(converter.program()) else {
                continue;
            };
            let result = converter
                .command(&program, &source.path, &output)
                .bounded_output(CONVERT_TIMEOUT);
            let made = match result {
                Ok(out) if out.status.success() => produced(&output),
                Ok(out) => {
                    failures.push(format!(
                        "{}: {}",
                        converter.program(),
                        first_line(&out.stderr)
                    ));
                    continue;
                }
                Err(e) => {
                    failures.push(format!("{}: {e:#}", converter.program()));
                    continue;
                }
            };
            let Some(png) = made else {
                failures.push(format!("{}: wrote no image", converter.program()));
                continue;
            };
            // Re-encode through `image` so the renderer gets a known-good PNG.
            let decoded = image::open(&png)
                .with_context(|| format!("decode {} output", converter.program()))?;
            let visual = ctx.workspace.join(format!("image-{}.png", source.id));
            decoded.save(&visual)?;
            let mut unit = Unit::visual(source.id, visual);
            unit.metadata
                .insert("heif.converter".into(), converter.program().into());
            return Ok(ImportOutcome {
                source,
                units: vec![unit],
                warnings: vec![],
            });
        }
        if failures.is_empty() {
            anyhow::bail!(
                "HEIC/HEIF/AVIF input requires sips (macOS), heif-convert (libheif) or ImageMagick"
            );
        }
        anyhow::bail!(
            "no converter could decode the image: {}",
            failures.join("; ")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinOptions, register_builtins};

    #[test]
    fn heif_family_is_claimed_by_extension_and_detected_type() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::default();
        register_builtins(&mut registry, BuiltinOptions::default());
        for name in ["IMG_0001.HEIC", "photo.heif", "pic.avif"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"not decoded during probing").unwrap();
            let source = SourceRecord::new(path);
            assert_eq!(HeifImporter.probe(&source), ProbeScore::EXTENSION, "{name}");
            assert_eq!(
                registry.importer_for(&source).unwrap().descriptor().name,
                "heif"
            );
        }
        let mut sniffed = SourceRecord::new(dir.path().join("download"));
        sniffed.detected_type = Some("image/heif".into());
        assert_eq!(HeifImporter.probe(&sniffed), ProbeScore::MAGIC);
    }

    #[test]
    fn converter_commands_select_the_primary_image_into_the_workspace() {
        let input = Path::new("in.heic");
        let output = Path::new("ws/out.png");
        let args = |c: Converter| -> Vec<String> {
            c.command(Path::new(c.program()), input, output)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        assert_eq!(
            args(Converter::Sips),
            ["-s", "format", "png", "in.heic", "--out", "ws/out.png"]
        );
        assert_eq!(args(Converter::HeifConvert), ["in.heic", "ws/out.png"]);
        assert_eq!(
            args(Converter::Magick),
            ["in.heic[0]", "-auto-orient", "ws/out.png"]
        );
        assert!(!Converter::Sips.usable_here() || cfg!(target_os = "macos"));
        assert!(!Converter::Convert.usable_here() || !cfg!(windows));
    }

    #[test]
    fn numbered_heif_convert_output_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("out.png");
        assert_eq!(produced(&output), None);
        std::fs::write(dir.path().join("out-1.png"), b"x").unwrap();
        assert_eq!(produced(&output), Some(dir.path().join("out-1.png")));
        std::fs::write(&output, b"x").unwrap();
        assert_eq!(produced(&output), Some(output));
    }

    #[test]
    fn undecodable_input_fails_with_a_provider_message() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.heic");
        std::fs::write(&path, b"definitely not an image").unwrap();
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let err = HeifImporter
            .import(&ctx, SourceRecord::new(path))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("requires sips") || err.contains("no converter could decode"),
            "{err}"
        );
    }
}
