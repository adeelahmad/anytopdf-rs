//! `anytopdf search` and `anytopdf index`: the cross-file SQLite search index.

use crate::cli::{IndexCommand, SearchArgs};
use crate::exit::{CliError, ExitClass, fail, tag};
use anyhow::Context;
use anytopdf_core::{ChunkSet, Manifest};
use anytopdf_index::{Document, Hit, Index, SearchQuery, default_index_path};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(crate) const SEARCH_SCHEMA: &str = "anytopdf.search/1";
pub(crate) const INDEX_SCHEMA: &str = "anytopdf.index/1";

/// The index named on the command line (or `ANYTOPDF_INDEX`), else the per-user one.
pub(crate) fn index_path(given: Option<&Path>) -> Result<PathBuf, CliError> {
    given
        .map(Path::to_path_buf)
        .or_else(|| default_index_path(|key| std::env::var(key).ok()))
        .ok_or_else(|| {
            fail(
                ExitClass::Usage,
                "no default index location (HOME is not set); pass --index-db",
            )
        })
}

/// Open (creating if needed) the index a conversion records into. Called before
/// rendering, so a bad index path fails the run before anything is published.
pub(crate) fn open_for_convert(given: Option<&Path>) -> Result<Index, CliError> {
    tag(ExitClass::Usage, Index::open(&index_path(given)?))
}

/// Record converted PDFs. The PDFs are already published, so a failure names them.
pub(crate) fn record(
    index: &mut Index,
    collection: Option<&str>,
    docs: Vec<(PathBuf, Document)>,
) -> Result<(), CliError> {
    for (pdf, mut doc) in docs {
        doc.pdf = std::path::absolute(&pdf)?;
        doc.collection = collection.map(str::to_owned);
        index.add(&doc).with_context(|| {
            format!(
                "wrote {} but could not record it in the search index {}",
                pdf.display(),
                index.path().display()
            )
        })?;
    }
    Ok(())
}

pub(crate) fn run_index(command: IndexCommand) -> Result<(), CliError> {
    match command {
        IndexCommand::Add {
            pdfs,
            collection,
            index_db,
            json,
        } => {
            let mut index = tag(
                ExitClass::Usage,
                Index::open(&index_path(index_db.as_deref())?),
            )?;
            let mut added = Vec::new();
            for pdf in &pdfs {
                added.push(add_pdf(&mut index, pdf, collection.as_deref())?);
            }
            if json {
                print_json(&json!({
                    "schema_version": INDEX_SCHEMA,
                    "index": index.path().display().to_string(),
                    "documents": added,
                }))?;
            } else {
                for doc in &added {
                    println!(
                        "{} {} ({} entries)",
                        if doc["skipped"] == true {
                            "unchanged"
                        } else {
                            "indexed"
                        },
                        doc["pdf"].as_str().unwrap_or_default(),
                        doc["entries"]
                    );
                }
            }
            Ok(())
        }
        IndexCommand::List {
            collection,
            index_db,
            json,
        } => {
            let index = tag(
                ExitClass::Input,
                Index::open_existing(&index_path(index_db.as_deref())?),
            )?;
            let docs = index.documents(collection.as_deref())?;
            if json {
                return print_json(&json!({
                    "schema_version": INDEX_SCHEMA,
                    "index": index.path().display().to_string(),
                    "documents": docs.iter().map(|d| summary_json(d, false)).collect::<Vec<_>>(),
                }));
            }
            for d in docs {
                println!(
                    "{}\t{}\t{} entries\t{}",
                    d.pdf,
                    d.collection.as_deref().unwrap_or("-"),
                    d.entries,
                    d.origin
                );
            }
            Ok(())
        }
        IndexCommand::Remove { pdfs, index_db } => {
            let mut index = tag(
                ExitClass::Input,
                Index::open_existing(&index_path(index_db.as_deref())?),
            )?;
            let mut missing = Vec::new();
            for pdf in &pdfs {
                let key = std::path::absolute(pdf)?;
                if !index.remove(&key)? {
                    missing.push(pdf.display().to_string());
                }
            }
            if missing.is_empty() {
                Ok(())
            } else {
                Err(fail(
                    ExitClass::Input,
                    format!("not in the index: {}", missing.join(", ")),
                ))
            }
        }
    }
}

fn summary_json(d: &anytopdf_index::DocumentSummary, skipped: bool) -> Value {
    json!({
        "pdf": d.pdf, "pdf_sha256": d.pdf_sha256, "origin": d.origin, "profile": d.profile,
        "collection": d.collection, "indexed_at": d.indexed_at, "sources": d.sources,
        "entries": d.entries, "skipped": skipped,
    })
}

/// Index one existing PDF from its embedded (or sidecar) manifest and chunks. A PDF
/// that `convert --index` already recorded with full annotations, and that has not
/// changed since, keeps that richer record.
fn add_pdf(index: &mut Index, pdf: &Path, collection: Option<&str>) -> Result<Value, CliError> {
    let key = tag(ExitClass::Input, std::path::absolute(pdf))?;
    let bytes = tag(
        ExitClass::Input,
        std::fs::read(pdf).with_context(|| format!("cannot read {}", pdf.display())),
    )?;
    let digest = anytopdf_core::sha256_hex(&bytes);
    let known = index.document(&key)?;
    // Without --collection a re-indexed PDF stays in its collection.
    let collection = collection
        .map(str::to_owned)
        .or_else(|| known.as_ref().and_then(|k| k.collection.clone()));
    if let Some(known) = known
        && known.pdf_sha256 == digest
        && known.origin == "convert"
    {
        index.set_collection(&key, collection.as_deref())?;
        let current = index.document(&key)?.unwrap_or(known);
        return Ok(summary_json(&current, true));
    }
    let extracted = crate::extract::extract(pdf)?;
    let manifest: Manifest = parse(pdf, &extracted["manifest"], "manifest")?;
    let chunks: ChunkSet = parse(pdf, &extracted["chunks"], "chunks")?;
    let mut doc = Document::from_chunks(&key, &bytes, &manifest, &chunks);
    doc.collection = collection;
    Ok(summary_json(&index.add(&doc)?, false))
}

fn parse<T: serde::de::DeserializeOwned>(
    pdf: &Path,
    value: &Value,
    what: &str,
) -> Result<T, CliError> {
    tag(
        ExitClass::Input,
        serde_json::from_value(value.clone())
            .with_context(|| format!("cannot read the {what} of {}", pdf.display())),
    )
}

pub(crate) fn search(args: SearchArgs) -> Result<(), CliError> {
    let text = Some(args.query.join(" ")).filter(|q| !q.trim().is_empty());
    if text.is_none() && args.kind.is_empty() && args.person.is_none() && args.collection.is_none()
    {
        return Err(fail(
            ExitClass::Usage,
            "give search text, or filter with --kind, --person or --collection",
        ));
    }
    let index = tag(
        ExitClass::Input,
        Index::open_existing(&index_path(args.index_db.as_deref())?),
    )?;
    let query = SearchQuery {
        text: text.clone(),
        kinds: args.kind.clone(),
        person: args.person.clone(),
        collection: args.collection.clone(),
        limit: args.limit,
    };
    let hits = index.search(&query)?;
    if args.json {
        return print_json(&json!({
            "schema_version": SEARCH_SCHEMA,
            "index": index.path().display().to_string(),
            "query": text,
            "hits": hits,
        }));
    }
    if hits.is_empty() {
        eprintln!("no matches");
    }
    for hit in &hits {
        println!("{}", hit_line(hit));
    }
    Ok(())
}

fn clock(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total / 60) % 60,
        total % 60
    )
}

/// One human-readable result: where (PDF, page, time, source), what, and the match.
fn hit_line(hit: &Hit) -> String {
    let mut place = hit.pdf.clone();
    if let Some(p) = &hit.pages {
        place.push_str(&format!(" p.{}", p.first));
    }
    if let Some(t) = &hit.time {
        place.push_str(&format!(" @{}", clock(t.start_seconds)));
    }
    let source = hit.source.name.as_deref().unwrap_or_default();
    let snippet = hit.snippet.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut line = format!("{place}  {}  {snippet}", hit.kind);
    if !source.is_empty() {
        line.push_str(&format!("  ({source})"));
    }
    line
}

fn print_json(value: &Value) -> Result<(), CliError> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use anytopdf_index::{HitSource, PageSpan, TimeSpan};

    #[test]
    fn hit_lines_name_pdf_page_time_kind_and_source() {
        let hit = Hit {
            pdf: "/out/m.pdf".into(),
            collection: None,
            source: HitSource {
                id: "s".into(),
                name: Some("meeting.mp4".into()),
                path: None,
            },
            unit_id: "u".into(),
            unit_kind: "visual".into(),
            kind: "face".into(),
            provider: None,
            confidence: None,
            text: "Alice".into(),
            snippet: "[Alice]".into(),
            pages: Some(PageSpan { first: 3, last: 3 }),
            time: Some(TimeSpan {
                start_seconds: 3725.4,
                end_seconds: 3725.4,
            }),
            region: None,
            frame: None,
            attributes: Default::default(),
        };
        assert_eq!(
            hit_line(&hit),
            "/out/m.pdf p.3 @01:02:05  face  [Alice]  (meeting.mp4)"
        );
    }
}
