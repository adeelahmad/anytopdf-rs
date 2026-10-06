use super::{Sink, report_diagnostic};
use crate::exit::{CliError, ExitClass, Redactor, fail, tag};
use anyhow::{Context, Result};
use anytopdf_core::{
    ChunkSet, Diagnostic, DiagnosticCode, Manifest, Pipeline, PipelineRun, Profile,
};
use anytopdf_pdf::{CHUNKS_FILE, EmbeddedFile, MANIFEST_FILE, embed_files, read_embedded_files};
use std::path::Path;

pub(super) struct StagedDocument {
    pub(super) bytes: Vec<u8>,
    pub(super) pages: usize,
    pub(super) sources: Vec<serde_json::Value>,
    pub(super) sidecars: Option<(Vec<u8>, Vec<u8>)>,
    pub(super) index: Option<anytopdf_index::Document>,
}

pub(super) fn stage_document(
    pipeline: &Pipeline,
    run: &PipelineRun,
    (renderer, strict, index): (&str, bool, bool),
    staged: &Path,
    redactor: &Redactor,
    sink: &mut Sink,
    profile: Profile,
) -> Result<StagedDocument, CliError> {
    let report = tag(ExitClass::Render, pipeline.render(run, renderer, staged))?;
    for warning in &report.warnings {
        report_diagnostic(
            sink,
            &Diagnostic::new(DiagnosticCode::RenderWarning, warning),
            redactor,
            profile,
        );
    }
    if strict && !report.warnings.is_empty() {
        return Err(fail(
            ExitClass::Strict,
            "strict conversion stopped on rendering warnings",
        ));
    }
    let bytes = tag(
        ExitClass::Render,
        std::fs::read(staged).context("renderer did not produce output"),
    )?;
    if !bytes.starts_with(b"%PDF-") || report.pages == 0 {
        return Err(fail(
            ExitClass::Render,
            "renderer did not produce a valid PDF",
        ));
    }
    let built = Manifest::build(&run.graph, &report);
    let sources: Vec<serde_json::Value> = built
        .sources
        .iter()
        .map(|s| {
            serde_json::json!({
                "input": s.path.clone().unwrap_or_else(|| s.name.clone()),
                "source_id": s.id.to_string(),
                "sha256": s.sha256,
            })
        })
        .collect();
    let manifest = serde_json::to_vec_pretty(&built)?;
    let chunks = serde_json::to_vec_pretty(&ChunkSet::build(&run.graph, &report))?;
    let files = [
        EmbeddedFile {
            name: MANIFEST_FILE.into(),
            mime_type: "application/json".into(),
            bytes: manifest.clone(),
        },
        EmbeddedFile {
            name: CHUNKS_FILE.into(),
            mime_type: "application/json".into(),
            bytes: chunks.clone(),
        },
    ];
    // A renderer that wrote these exact attachments itself (pdfa stores them as
    // PDF/A-3 associated files) must not be rewritten.
    let embedded = if already_embedded(&bytes, &files) {
        Ok(None)
    } else {
        embed_files(&bytes, &files).map(Some)
    };
    let (bytes, sidecars) = match embedded {
        Ok(None) => (bytes, None),
        Ok(Some(embedded)) => (embedded, None),
        Err(e) => {
            report_diagnostic(
                sink,
                &Diagnostic::new(
                    DiagnosticCode::ManifestSidecar,
                    format!("could not embed manifest and chunks ({e}); writing sidecar files"),
                ),
                redactor,
                profile,
            );
            (bytes, Some((manifest, chunks)))
        }
    };
    // The published path is not known yet; `search::record` sets it.
    let index =
        index.then(|| anytopdf_index::Document::from_graph(staged, &bytes, &run.graph, &report));
    Ok(StagedDocument {
        bytes,
        pages: report.pages,
        sources,
        sidecars,
        index,
    })
}

fn already_embedded(pdf: &[u8], files: &[EmbeddedFile]) -> bool {
    let Ok(present) = read_embedded_files(pdf) else {
        return false;
    };
    files.iter().all(|want| {
        present
            .iter()
            .any(|have| have.name == want.name && have.bytes == want.bytes)
    })
}
