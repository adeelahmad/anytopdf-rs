use crate::convert::registry;
use crate::exit::{CliError, ExitClass, fail, tag};
use anyhow::{Context, Result};
use anytopdf_builtin::{BuiltinOptions, OcrEnricher, capture::capture_status, detect_providers};
use anytopdf_core::{RuntimePlugin, RuntimePluginPolicy, discover_runtime_plugins_detailed};
use anytopdf_plugin_whisper::{backend, setup};
use std::path::{Path, PathBuf};

/// Whether audio and video get transcribed, and what is missing if not.
pub(crate) struct Transcription {
    pub(crate) available: bool,
    pub(crate) plugin: Option<PathBuf>,
    pub(crate) model: Option<PathBuf>,
    pub(crate) detail: String,
}

impl Transcription {
    pub(crate) fn detect(policy: &RuntimePluginPolicy) -> Self {
        let found = discover_runtime_plugins_detailed(policy);
        let whisper = |plugins: &[RuntimePlugin]| {
            plugins
                .iter()
                .find(|p| p.manifest.name == "whisper")
                .cloned()
        };
        let readiness =
            || backend::readiness(&backend::Config::from_env(), |name| which::which(name).ok());
        Self::from_parts(
            policy.enabled,
            whisper(&found.plugins),
            whisper(&found.idle),
            std::env::var_os("ANYTOPDF_WHISPER_MODEL")
                .map(PathBuf::from)
                .filter(|p| p.is_file())
                .or_else(setup::recorded_model),
            readiness,
        )
    }

    fn from_parts(
        enabled: bool,
        active: Option<RuntimePlugin>,
        idle: Option<RuntimePlugin>,
        model: Option<PathBuf>,
        readiness: impl FnOnce() -> backend::Readiness,
    ) -> Self {
        let (available, plugin, detail) = if !enabled {
            (
                false,
                None,
                "runtime plugins are disabled by --no-plugins".into(),
            )
        } else if let Some(plugin) = active.as_ref().filter(|p| p.manifest.ready) {
            let detail = plugin.manifest.detail.clone().unwrap_or_default();
            (true, Some(plugin.executable.clone()), detail)
        } else if let Some(plugin) = active.or(idle) {
            let detail = plugin
                .manifest
                .detail
                .clone()
                .unwrap_or_else(|| "the Whisper plugin is not ready".into());
            (false, Some(plugin.executable), detail)
        } else {
            let readiness = readiness();
            let mut detail = "anytopdf-plugin-whisper not found in plugins/ beside anytopdf, \
                              on PATH or in ANYTOPDF_PLUGIN_PATH"
                .to_string();
            if !readiness.ready {
                detail.push_str("; also ");
                detail.push_str(&readiness.detail);
            }
            (false, None, detail)
        };
        Transcription {
            available,
            plugin,
            model,
            detail,
        }
    }
}

pub(crate) fn doctor(json: bool, policy: &RuntimePluginPolicy) -> Result<()> {
    let transcription = Transcription::detect(policy);
    let path = |p: &Option<PathBuf>| p.as_ref().map(|p| p.display().to_string());
    if json {
        let providers: Vec<_> = detect_providers()
            .into_iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name, "available": p.available,
                    "path": p.path.map(|path| path.display().to_string()),
                    "version": p.version
                })
            })
            .collect();
        let ocr: Vec<_> = OcrEnricher::status()
            .into_iter()
            .map(|s| serde_json::json!({"name": s.name, "available": s.available, "detail": s.detail}))
            .collect();
        let printing: Vec<_> = crate::print::printing_status()
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "name": s.name, "available": s.available,
                    "path": s.path.map(|path| path.display().to_string()),
                    "detail": s.detail
                })
            })
            .collect();
        let capture: Vec<_> = capture_status()
            .into_iter()
            .map(|s| serde_json::json!({"name": s.name, "available": s.available, "detail": s.detail}))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": "anytopdf.doctor/1", "providers": providers, "ocr": ocr,
                "printing": printing, "capture": capture,
                "transcription": {
                    "available": transcription.available,
                    "plugin": path(&transcription.plugin),
                    "model": path(&transcription.model),
                    "detail": transcription.detail
                }
            }))?
        );
        return Ok(());
    }
    println!("anytopdf provider diagnostics");
    println!();

    for p in detect_providers() {
        match p.path {
            Some(path) => println!(
                "[ok]   {:<10} {} {}",
                p.name,
                path.display(),
                p.version.unwrap_or_default()
            ),
            None => println!("[miss] {:<10}", p.name),
        }
    }

    println!();
    println!("OCR:");
    for status in OcrEnricher::status() {
        println!(
            "{} {:<10} {}",
            if status.available { "[ok]  " } else { "[miss]" },
            status.name,
            status.detail
        );
    }

    println!();
    println!("Printing:");
    for status in crate::print::printing_status() {
        println!(
            "{} {:<16} {}{}",
            if status.available { "[ok]  " } else { "[miss]" },
            status.name,
            status
                .path
                .map(|p| format!("{} ", p.display()))
                .unwrap_or_default(),
            status.detail
        );
    }

    println!();
    println!("Screen capture:");
    for status in capture_status() {
        println!(
            "{} {:<10} {}",
            if status.available { "[ok]  " } else { "[miss]" },
            status.name,
            status.detail
        );
    }

    println!();
    println!("Transcription:");
    println!(
        "{} {:<10} {}",
        if transcription.available {
            "[ok]  "
        } else {
            "[miss]"
        },
        "whisper",
        transcription.detail
    );
    if let Some(plugin) = &transcription.plugin {
        println!("       plugin     {}", plugin.display());
    }
    match &transcription.model {
        Some(model) => println!("       model      {}", model.display()),
        None if !transcription.available => {
            println!("       model      none installed; run `anytopdf setup whisper`")
        }
        None => {}
    }
    Ok(())
}

pub(crate) fn plugins(policy: &RuntimePluginPolicy, json: bool) -> Result<()> {
    let (registry, warnings) = registry(BuiltinOptions::default(), policy);
    for warning in warnings {
        eprintln!("WARNING: {warning}");
    }
    let mut descriptors = registry.descriptors();
    descriptors.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "schema_version": "anytopdf.plugins/1", "plugins": descriptors
            }))?
        );
        return Ok(());
    }

    for p in descriptors {
        println!(
            "{:<17} {:<24} priority={:<4} ext={}",
            p.kind,
            p.name,
            p.priority,
            if p.extensions.is_empty() {
                "-".into()
            } else {
                p.extensions.join(",")
            }
        );
    }
    Ok(())
}

pub(crate) fn probe(path: &Path, policy: &RuntimePluginPolicy) -> Result<(), CliError> {
    let path = tag(
        ExitClass::Input,
        path.canonicalize()
            .with_context(|| format!("input not found: {}", path.display())),
    )?;
    if !path.is_file() {
        return Err(fail(ExitClass::Input, "probe requires a file"));
    }
    let (registry, warnings) = registry(BuiltinOptions::default(), policy);
    for warning in warnings {
        eprintln!("WARNING: {warning}");
    }
    let mut source = anytopdf_core::SourceRecord::new(path);
    if let Some(kind) = infer::get_from_path(&source.path)? {
        source.detected_type = Some(kind.mime_type().to_string());
    }
    let importer = registry.importer_for(&source)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "schema_version": "anytopdf.probe/1",
            "source": source, "importer": importer.descriptor()
        }))?
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_core::{RuntimeCapability, RuntimePluginManifest};
    use std::time::Duration;

    fn whisper(ready: bool, detail: &str) -> RuntimePlugin {
        RuntimePlugin {
            timeout: Duration::from_secs(1),
            sandbox: Default::default(),
            options: Default::default(),
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
                ready,
                detail: Some(detail.into()),
            },
        }
    }

    fn unused() -> backend::Readiness {
        panic!("the plugin's own manifest already answers this")
    }

    #[test]
    fn transcription_reports_the_plugins_own_view() {
        let ready = Transcription::from_parts(
            true,
            Some(whisper(true, "whisper.cpp:ggml-base via /bin/whisper-cli")),
            None,
            Some(PathBuf::from("/data/whisper/ggml-base.bin")),
            unused,
        );
        assert!(ready.available);
        assert_eq!(ready.detail, "whisper.cpp:ggml-base via /bin/whisper-cli");

        let idle = Transcription::from_parts(
            true,
            None,
            Some(whisper(false, "no Whisper engine found")),
            None,
            unused,
        );
        assert!(!idle.available);
        assert_eq!(idle.detail, "no Whisper engine found");
        assert!(idle.plugin.is_some());

        // A plugin on ANYTOPDF_PLUGIN_PATH runs even when not ready; still not available.
        let explicit =
            Transcription::from_parts(true, Some(whisper(false, "no model")), None, None, unused);
        assert!(!explicit.available);
        assert_eq!(explicit.detail, "no model");
    }

    #[test]
    fn transcription_without_the_plugin_names_every_missing_piece() {
        let missing = Transcription::from_parts(true, None, None, None, || backend::Readiness {
            ready: false,
            detail: "no Whisper engine found".into(),
        });
        assert!(!missing.available);
        assert!(missing.detail.contains("anytopdf-plugin-whisper not found"));
        assert!(missing.detail.contains("no Whisper engine found"));

        let disabled = Transcription::from_parts(false, None, None, None, unused);
        assert!(disabled.detail.contains("--no-plugins"));
    }
}
