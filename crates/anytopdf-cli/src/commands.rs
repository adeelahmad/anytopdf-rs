use crate::convert::registry;
use crate::exit::{CliError, ExitClass, fail, tag};
use anyhow::{Context, Result};
use anytopdf_builtin::{BuiltinOptions, OcrEnricher, capture::capture_status, detect_providers};
use anytopdf_core::RuntimePluginPolicy;
use std::path::Path;

pub(crate) fn doctor(json: bool) -> Result<()> {
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
                "printing": printing, "capture": capture
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
