//! Human-readable `capabilities` table: what this binary and environment support.

use crate::convert::registry;
use anytopdf_builtin::{
    BuiltinOptions, OcrEnricher, OcrProviderStatus, ProviderVersion, detect_providers,
};
use anytopdf_core::{
    PluginDescriptor, RuntimePlugin, RuntimePluginPolicy, discover_runtime_plugins_detailed,
};
use std::fmt::Write;

/// Footer for `capabilities --help`; the table itself prints environment-specific hints.
pub(crate) const HELP_FOOTER: &str = "\
Without --json, prints a table of what this binary and environment support:
  A = available   P = partial (some optional tools missing)   - = missing
  B = built into this binary   R = runtime plugin (anytopdf-plugin-*)

Enable missing capabilities:
  Install the external tool named in the table so it is on PATH: FFmpeg
  (ffmpeg, ffprobe) for video and embedded subtitles, ExifTool for EXIF/XMP
  metadata, Tesseract or Python docTR (pip install python-doctr) for OCR.
  Apple Vision OCR needs a macOS build with the default `apple-vision` feature.

Add new capabilities:
  Write an executable named anytopdf-plugin-<name> in any language that speaks
  the JSON protocol in PLUGIN_PROTOCOL.md (see examples/anytopdf-plugin-example.py),
  then put it on PATH or in a directory listed in ANYTOPDF_PLUGIN_PATH.
  It appears here and in `anytopdf plugins` once its manifest is valid.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Available,
    Partial,
    Missing,
}

impl State {
    fn flag(self) -> char {
        match self {
            State::Available => 'A',
            State::Partial => 'P',
            State::Missing => '-',
        }
    }
}

#[derive(Debug)]
pub(crate) struct Entry {
    pub(crate) state: State,
    pub(crate) runtime: bool,
    pub(crate) name: String,
    pub(crate) detail: String,
}

#[derive(Debug, Default)]
pub(crate) struct Report {
    pub(crate) sections: Vec<(&'static str, Vec<Entry>)>,
    pub(crate) plugin_note: String,
    pub(crate) hints: Vec<String>,
}

/// Everything the report is built from, gathered separately so it can be faked in tests.
pub(crate) struct Probe {
    pub(crate) tools: Vec<ProviderVersion>,
    pub(crate) ocr: Vec<OcrProviderStatus>,
    pub(crate) builtins: Vec<PluginDescriptor>,
    pub(crate) plugins: Vec<RuntimePlugin>,
    /// Bundled plugins waiting on a dependency (manifest `ready: false`).
    pub(crate) idle: Vec<RuntimePlugin>,
    pub(crate) ignored: Vec<String>,
    pub(crate) policy: RuntimePluginPolicy,
    pub(crate) plugin_path: Option<String>,
}

impl Probe {
    pub(crate) fn detect(policy: &RuntimePluginPolicy) -> Self {
        let disabled = RuntimePluginPolicy {
            enabled: false,
            ..policy.clone()
        };
        let (registry, _) = registry(BuiltinOptions::default(), &disabled);
        let found = discover_runtime_plugins_detailed(policy);
        Probe {
            tools: detect_providers(),
            ocr: OcrEnricher::status(),
            builtins: registry.descriptors(),
            plugins: found.plugins,
            idle: found.idle,
            ignored: found.warnings,
            policy: policy.clone(),
            plugin_path: std::env::var_os("ANYTOPDF_PLUGIN_PATH")
                .map(|p| p.to_string_lossy().into_owned()),
        }
    }

    fn tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name == name && t.available)
    }
}

fn install_hint(tool: &str) -> String {
    let (brew, apt, winget) = match tool {
        "ffmpeg" | "ffprobe" => ("ffmpeg", "ffmpeg", "Gyan.FFmpeg"),
        "exiftool" => ("exiftool", "libimage-exiftool-perl", "OliverBetz.ExifTool"),
        "tesseract" => ("tesseract", "tesseract-ocr", "UB-Mannheim.TesseractOCR"),
        "python3" => ("python", "python3", "Python.Python.3.12"),
        "heif-convert" => ("libheif", "libheif-examples", "ImageMagick.ImageMagick"),
        "pdftoppm" => ("poppler", "poppler-utils", "oschwartz10612.Poppler"),
        "yt-dlp" => ("yt-dlp", "yt-dlp", "yt-dlp.yt-dlp"),
        "chrome" => ("--cask chromium", "chromium", "Google.Chrome"),
        other => return format!("install {other} and put it on PATH"),
    };
    let command = if cfg!(target_os = "macos") {
        format!("brew install {brew}")
    } else if cfg!(windows) {
        format!("winget install {winget}")
    } else {
        format!("apt install {apt} (or your package manager)")
    };
    format!("install {tool}: {command}")
}

fn yes_no(ok: bool) -> &'static str {
    if ok { "yes" } else { "no" }
}

fn extensions(d: &PluginDescriptor) -> String {
    if d.extensions.is_empty() {
        String::new()
    } else {
        format!(
            ".{}",
            d.extensions
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" .")
        )
    }
}

fn kind_label(kind: &str) -> &str {
    kind.strip_suffix("-enricher").unwrap_or(kind)
}

fn section_for(kind: &str) -> usize {
    match kind {
        "importer" => 0,
        "renderer" => 2,
        _ => 1,
    }
}

fn missing_tools(probe: &Probe, tools: &[&str], hints: &mut Vec<String>) -> Vec<&'static str> {
    let missing: Vec<&'static str> = ["ffmpeg", "ffprobe", "exiftool", "tesseract"]
        .into_iter()
        .filter(|t| tools.contains(t) && !probe.tool(t))
        .collect();
    for tool in &missing {
        hints.push(install_hint(tool));
    }
    missing
}

/// Classify one built-in plugin against the tools it shells out to.
fn builtin_entry(probe: &Probe, d: &PluginDescriptor, hints: &mut Vec<String>) -> Entry {
    let mut need = |tools: &[&str]| missing_tools(probe, tools, hints);
    let exts = extensions(d);
    let (state, detail) = match d.name.as_str() {
        "ffmpeg-video" => match need(&["ffmpeg"]).as_slice() {
            [] => (State::Available, exts),
            _ => (State::Missing, format!("needs ffmpeg; {exts}")),
        },
        "heif" => {
            let converters: &[&str] = if cfg!(target_os = "macos") {
                &["sips", "heif-convert", "magick", "convert"]
            } else if cfg!(windows) {
                &["heif-convert", "magick"]
            } else {
                &["heif-convert", "magick", "convert"]
            };
            match converters.iter().find(|c| which::which(c).is_ok()) {
                Some(found) => (State::Available, format!("{exts}  (via {found})")),
                None => {
                    hints.push(install_hint("heif-convert"));
                    (
                        State::Missing,
                        format!("needs heif-convert (libheif) or ImageMagick; {exts}"),
                    )
                }
            }
        }
        "camera-raw" => {
            let developers: &[&str] = if cfg!(target_os = "macos") {
                &["sips", "dcraw_emu", "dcraw", "magick", "convert"]
            } else if cfg!(windows) {
                &["dcraw_emu", "dcraw", "magick"]
            } else {
                &["dcraw_emu", "dcraw", "magick", "convert"]
            };
            // The embedded camera preview needs no tool; developing does.
            let via = developers.iter().find(|c| which::which(c).is_ok()).map_or(
                "embedded previews only; develop with LibRaw or ImageMagick",
                |d| d,
            );
            (State::Available, format!("{exts}  (via {via})"))
        }
        "scan" => (
            State::Available,
            "unit    page detection, perspective and deskew".into(),
        ),
        "pdf-input" => {
            if which::which("pdftoppm").is_ok() {
                (
                    State::Available,
                    format!("{exts}  (page images via pdftoppm)"),
                )
            } else {
                hints.push(install_hint("pdftoppm"));
                (
                    State::Partial,
                    format!("{exts}  (text only; page images need Poppler pdftoppm)"),
                )
            }
        }
        "audio" => (
            State::Available,
            format!("{exts}  (placeholder pages; text via --transcript or a plugin)"),
        ),
        "metadata" => {
            let missing = need(&["exiftool", "ffprobe"]);
            let state = if missing.is_empty() {
                State::Available
            } else {
                State::Partial
            };
            let detail = format!(
                "source  file facts; exiftool: {}, ffprobe: {}",
                yes_no(probe.tool("exiftool")),
                yes_no(probe.tool("ffprobe"))
            );
            (state, detail)
        }
        "captions-transcripts" => {
            let missing = need(&["ffmpeg", "ffprobe"]);
            if missing.is_empty() {
                (
                    State::Available,
                    "graph   sidecar and embedded subtitles".into(),
                )
            } else {
                (
                    State::Partial,
                    format!(
                        "graph   sidecar subtitles; embedded ones need {}",
                        missing.join(", ")
                    ),
                )
            }
        }
        "ocr-auto" => {
            let any = probe.ocr.iter().any(|s| s.available);
            let list = probe
                .ocr
                .iter()
                .map(|s| format!("{}: {}", s.name, yes_no(s.available)))
                .collect::<Vec<_>>()
                .join(", ");
            if !any {
                hints.push(install_hint("tesseract"));
            }
            let state = if any {
                State::Available
            } else {
                State::Missing
            };
            (state, format!("unit    OCR chain; {list}"))
        }
        _ if d.kind.ends_with("-enricher") => (
            State::Available,
            format!("{:<7} {exts}", kind_label(&d.kind))
                .trim_end()
                .into(),
        ),
        _ => (State::Available, exts),
    };
    Entry {
        state,
        runtime: false,
        name: d.name.clone(),
        detail,
    }
}

fn ocr_entry(status: &OcrProviderStatus, hints: &mut Vec<String>) -> Entry {
    if !status.available {
        match status.name {
            "vision" if cfg!(target_os = "macos") => hints.push(
                "enable Apple Vision OCR: build with the default `apple-vision` feature".into(),
            ),
            "doctr" => {
                if which::which("python3")
                    .or_else(|_| which::which("python"))
                    .is_err()
                {
                    hints.push(install_hint("python3"));
                }
                hints.push("enable docTR OCR: python3 -m pip install python-doctr".into())
            }
            "tesseract" => hints.push(install_hint("tesseract")),
            _ => {}
        }
    }
    Entry {
        state: if status.available {
            State::Available
        } else {
            State::Missing
        },
        runtime: false,
        name: status.name.to_string(),
        detail: status.detail.clone(),
    }
}

fn tool_entry(tool: &ProviderVersion) -> Entry {
    let detail = match &tool.path {
        Some(path) => format!(
            "{} {}",
            tool.version.as_deref().unwrap_or("?"),
            path.display()
        ),
        None => "not found on PATH".into(),
    };
    Entry {
        state: if tool.available {
            State::Available
        } else {
            State::Missing
        },
        runtime: false,
        name: tool.name.to_string(),
        detail,
    }
}

pub(crate) fn build(probe: &Probe) -> Report {
    let mut hints = Vec::new();
    let mut kinds: [Vec<Entry>; 3] = Default::default();
    let mut builtins = probe.builtins.clone();
    builtins.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    for d in &builtins {
        kinds[section_for(&d.kind)].push(builtin_entry(probe, d, &mut hints));
    }
    for plugin in &probe.plugins {
        for cap in &plugin.manifest.capabilities {
            let permitted = probe.policy.permits(&cap.kind);
            let d = PluginDescriptor {
                name: String::new(),
                version: String::new(),
                kind: cap.kind.clone(),
                extensions: cap.extensions.clone(),
                mime_types: cap.mime_types.clone(),
                priority: cap.priority,
            };
            let mut detail = format!("{} {}", plugin.manifest.version, extensions(&d));
            if cap.kind != "importer" && cap.kind != "renderer" {
                detail = format!("{:<7} {detail}", kind_label(&cap.kind));
            }
            if !permitted {
                detail.push_str("  (blocked by --allow/--deny-plugin-kind)");
            }
            detail.push_str(&format!("  [{}]", plugin.executable.display()));
            kinds[section_for(&cap.kind)].push(Entry {
                state: if permitted {
                    State::Available
                } else {
                    State::Missing
                },
                runtime: true,
                name: format!("runtime:{}", plugin.manifest.name),
                detail,
            });
        }
    }
    for plugin in &probe.idle {
        let detail = plugin
            .manifest
            .detail
            .as_deref()
            .unwrap_or("not ready in this environment");
        for cap in &plugin.manifest.capabilities {
            kinds[section_for(&cap.kind)].push(Entry {
                state: State::Missing,
                runtime: true,
                name: format!("runtime:{}", plugin.manifest.name),
                detail: format!(
                    "{:<7} {} bundled, off until ready  [{}]",
                    kind_label(&cap.kind),
                    plugin.manifest.version,
                    plugin.executable.display()
                ),
            });
        }
        hints.push(format!(
            "turn on runtime:{}: {detail}",
            plugin.manifest.name
        ));
    }
    let [importers, enrichers, renderers] = kinds;
    let ocr = probe.ocr.iter().map(|s| ocr_entry(s, &mut hints)).collect();
    let tools = probe.tools.iter().map(tool_entry).collect();
    // URL inputs: yt-dlp fetches video and podcast links, Chrome snapshots web pages.
    for tool in &probe.tools {
        if matches!(tool.name, "yt-dlp" | "chrome") && !tool.available {
            hints.push(install_hint(tool.name));
        }
    }
    let mut sections = vec![
        ("Importers", importers),
        ("Enrichers", enrichers),
        ("Renderers", renderers),
        ("OCR providers", ocr),
        ("External tools", tools),
    ];
    if !probe.ignored.is_empty() {
        let ignored = probe
            .ignored
            .iter()
            .map(|warning| Entry {
                state: State::Missing,
                runtime: true,
                name: "ignored".into(),
                detail: warning.clone(),
            })
            .collect();
        sections.push(("Runtime plugins ignored", ignored));
        hints.push("fix ignored runtime plugins: each must print a protocol-1 manifest for --anytopdf-manifest (PLUGIN_PROTOCOL.md)".into());
    }
    let plugin_note = if !probe.policy.enabled {
        "runtime plugins: disabled by --no-plugins".to_string()
    } else {
        format!(
            "runtime plugins: {} found in PATH, ANYTOPDF_PLUGIN_PATH ({}) and beside anytopdf",
            probe.plugins.len(),
            probe.plugin_path.as_deref().unwrap_or("unset")
        )
    };
    let mut seen = std::collections::BTreeSet::new();
    hints.retain(|h| seen.insert(h.clone()));
    Report {
        sections,
        plugin_note,
        hints,
    }
}

pub(crate) fn render(report: &Report) -> String {
    let mut out = String::new();
    let features = if cfg!(feature = "apple-vision") {
        "apple-vision"
    } else {
        "none"
    };
    let _ = writeln!(
        out,
        "anytopdf {} ({}-{}; features: {features})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(out, "{}", report.plugin_note);
    out.push_str(
        "Capabilities:\n A. = available\n P. = partial (some optional tools missing)\n \
         -. = missing\n .B = built into this binary\n .R = runtime plugin (anytopdf-plugin-*)\n ---\n",
    );
    for (title, entries) in &report.sections {
        let _ = writeln!(out, "{title}:");
        if entries.is_empty() {
            out.push_str("  (none)\n");
        }
        for e in entries {
            let origin = if title.starts_with("OCR") || title.starts_with("External") {
                ' '
            } else if e.runtime {
                'R'
            } else {
                'B'
            };
            let line = format!(" {}{} {:<22} {}", e.state.flag(), origin, e.name, e.detail);
            let _ = writeln!(out, "{}", line.trim_end());
        }
    }
    out.push('\n');
    if report.hints.is_empty() {
        out.push_str("Everything built in is available in this environment.\n");
    } else {
        out.push_str("Enable missing capabilities:\n");
        for hint in &report.hints {
            let _ = writeln!(out, "  - {hint}");
        }
    }
    out.push_str(
        "Add new capabilities:\n  - write an anytopdf-plugin-<name> executable that speaks \
         PLUGIN_PROTOCOL.md\n    (example: examples/anytopdf-plugin-example.py) and put it on \
         PATH or ANYTOPDF_PLUGIN_PATH\n  - run `anytopdf capabilities --help` for details, \
         `--json` for machine-readable output\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_core::{RuntimeCapability, RuntimePluginManifest};
    use std::{path::PathBuf, time::Duration};

    fn descriptor(kind: &str, name: &str, exts: &[&str]) -> PluginDescriptor {
        PluginDescriptor {
            name: name.into(),
            version: "0".into(),
            kind: kind.into(),
            extensions: exts.iter().map(|e| e.to_string()).collect(),
            mime_types: vec![],
            priority: 0,
        }
    }

    fn tool(name: &'static str, found: bool) -> ProviderVersion {
        ProviderVersion {
            name,
            available: found,
            path: found.then(|| PathBuf::from(format!("/bin/{name}"))),
            version: found.then(|| "1.0".into()),
        }
    }

    fn probe(found: bool) -> Probe {
        Probe {
            tools: [
                "ffmpeg",
                "ffprobe",
                "exiftool",
                "tesseract",
                "python3",
                "yt-dlp",
                "chrome",
            ]
            .into_iter()
            .map(|n| tool(n, found))
            .collect(),
            ocr: vec![OcrProviderStatus {
                name: "tesseract",
                available: found,
                detail: "Tesseract CLI provider".into(),
            }],
            builtins: vec![
                descriptor("importer", "text", &["txt", "md"]),
                descriptor("importer", "ffmpeg-video", &["mp4"]),
                descriptor("source-enricher", "metadata", &[]),
                descriptor("unit-enricher", "ocr-auto", &[]),
                descriptor("renderer", "pdf", &["pdf"]),
            ],
            plugins: vec![],
            idle: vec![],
            ignored: vec![],
            policy: RuntimePluginPolicy::default(),
            plugin_path: None,
        }
    }

    fn entry<'a>(report: &'a Report, section: &str, name: &str) -> &'a Entry {
        report
            .sections
            .iter()
            .find(|(title, _)| *title == section)
            .and_then(|(_, entries)| entries.iter().find(|e| e.name == name))
            .unwrap_or_else(|| panic!("no {name} in {section}"))
    }

    #[test]
    fn missing_tools_mark_dependent_plugins_and_add_install_hints() {
        let report = build(&probe(false));
        assert_eq!(entry(&report, "Importers", "text").state, State::Available);
        let video = entry(&report, "Importers", "ffmpeg-video");
        assert_eq!(video.state, State::Missing);
        assert!(video.detail.contains("needs ffmpeg"), "{}", video.detail);
        assert_eq!(
            entry(&report, "Enrichers", "metadata").state,
            State::Partial
        );
        assert_eq!(
            entry(&report, "Enrichers", "ocr-auto").state,
            State::Missing
        );
        for tool in ["ffmpeg", "exiftool", "tesseract", "yt-dlp", "chrome"] {
            assert!(
                report
                    .hints
                    .iter()
                    .any(|h| h.starts_with(&format!("install {tool}:"))),
                "no hint for {tool}: {:?}",
                report.hints
            );
        }
        let unique: std::collections::BTreeSet<_> = report.hints.iter().collect();
        assert_eq!(
            unique.len(),
            report.hints.len(),
            "hints must be deduplicated"
        );
    }

    #[test]
    fn fully_provisioned_environment_needs_no_hints() {
        let report = build(&probe(true));
        assert!(report.hints.is_empty(), "{:?}", report.hints);
        assert_eq!(
            entry(&report, "Importers", "ffmpeg-video").state,
            State::Available
        );
        let text = render(&report);
        assert!(text.contains("Everything built in is available"), "{text}");
        assert!(text.contains(" AB text "), "{text}");
    }

    #[test]
    fn runtime_plugins_are_listed_by_kind_with_policy_and_path() {
        let mut probe = probe(true);
        probe.policy.deny_capabilities = ["unit-enricher".to_string()].into();
        probe.plugins.push(RuntimePlugin {
            timeout: Duration::from_secs(1),
            sandbox: Default::default(),
            executable: PathBuf::from("/opt/anytopdf-plugin-igl"),
            manifest: RuntimePluginManifest {
                protocol: 1,
                name: "igl".into(),
                version: "0.3.0".into(),
                capabilities: ["importer", "unit-enricher"]
                    .into_iter()
                    .map(|kind| RuntimeCapability {
                        kind: kind.into(),
                        extensions: vec!["igl".into()],
                        mime_types: vec![],
                        priority: 50,
                        phase: None,
                    })
                    .collect(),
                ready: true,
                detail: None,
            },
        });
        let report = build(&probe);
        let importer = entry(&report, "Importers", "runtime:igl");
        assert_eq!(importer.state, State::Available);
        assert!(importer.runtime);
        assert!(importer.detail.contains("/opt/anytopdf-plugin-igl"));
        let enricher = entry(&report, "Enrichers", "runtime:igl");
        assert_eq!(enricher.state, State::Missing);
        assert!(enricher.detail.contains("blocked"), "{}", enricher.detail);
        assert!(render(&report).contains(" AR runtime:igl"));
    }

    #[test]
    fn idle_bundled_plugins_say_what_turns_them_on() {
        let mut probe = probe(true);
        probe.idle.push(RuntimePlugin {
            timeout: Duration::from_secs(1),
            sandbox: Default::default(),
            executable: PathBuf::from("/opt/anytopdf/plugins/anytopdf-plugin-whisper"),
            manifest: RuntimePluginManifest {
                protocol: 1,
                name: "whisper".into(),
                version: "0.2.0".into(),
                capabilities: vec![RuntimeCapability {
                    kind: "graph-enricher".into(),
                    extensions: vec![],
                    mime_types: vec!["audio/*".into()],
                    priority: 50,
                    phase: None,
                }],
                ready: false,
                detail: Some("whisper.cpp has no ggml model; run `anytopdf setup whisper`".into()),
            },
        });
        let report = build(&probe);
        let entry = entry(&report, "Enrichers", "runtime:whisper");
        assert_eq!(entry.state, State::Missing);
        assert!(entry.detail.contains("off until ready"), "{}", entry.detail);
        assert!(
            report
                .hints
                .iter()
                .any(|h| h.starts_with("turn on runtime:whisper:") && h.contains("setup whisper")),
            "{:?}",
            report.hints
        );
        assert!(render(&report).contains(" -R runtime:whisper"));
    }

    #[test]
    fn disabled_and_ignored_plugins_are_reported() {
        let mut probe = probe(true);
        probe.policy.enabled = false;
        probe
            .ignored
            .push("runtime plugin /x/anytopdf-plugin-bad ignored: bad".into());
        let report = build(&probe);
        assert!(report.plugin_note.contains("--no-plugins"));
        let text = render(&report);
        assert!(text.contains("Runtime plugins ignored:"), "{text}");
        assert!(text.contains("anytopdf-plugin-bad"), "{text}");
        assert!(text.contains("PLUGIN_PROTOCOL.md"), "{text}");
    }
}
