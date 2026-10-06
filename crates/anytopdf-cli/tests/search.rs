use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

#[path = "common/process.rs"]
mod process;
#[path = "common/schema.rs"]
mod schema;
#[path = "common/stdout_json.rs"]
mod stdout_json;

use process::command;
use schema::validation_errors;
use stdout_json::single_document;

fn run(dir: &Path, index: &Path, args: &[&str]) -> Output {
    command()
        .args(args)
        .env("ANYTOPDF_INDEX", index)
        .env_remove("SOURCE_DATE_EPOCH")
        .current_dir(dir)
        .output()
        .unwrap()
}

fn ok(out: Output) -> Output {
    assert!(
        out.status.success(),
        "exit {:?}: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn json_of(out: Output, schema: &str) -> Value {
    let doc = single_document(&ok(out).stdout);
    let errors = validation_errors(schema, &doc);
    assert!(errors.is_empty(), "{schema}: {errors:?}\n{doc:#}");
    doc
}

fn convert(dir: &Path, index: &Path, name: &str, text: &str, extra: &[&str]) -> PathBuf {
    fs::write(dir.join(name), text).unwrap();
    let pdf = dir.join(format!("{name}.pdf"));
    let mut args = vec!["convert", name, "--ocr", "off", "-q", "-o"];
    let out = pdf.to_str().unwrap().to_string();
    args.push(&out);
    args.extend_from_slice(extra);
    ok(run(dir, index, &args));
    pdf
}

#[test]
fn convert_index_records_output_that_search_finds_with_page_and_source() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let index = dir.join("db/index.sqlite");
    let pdf = convert(
        dir,
        &index,
        "invoice.txt",
        "Quarterly invoice from Acme\nTotal due 42\n",
        &["--index", "--collection", "work"],
    );
    convert(
        dir,
        &index,
        "other.txt",
        "Nothing relevant here\n",
        &["--index"],
    );

    let doc = json_of(run(dir, &index, &["search", "invoice", "--json"]), "search");
    assert_eq!(doc["schema_version"], "anytopdf.search/1");
    let hits = doc["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1, "{doc:#}");
    let hit = &hits[0];
    assert_eq!(
        Path::new(hit["pdf"].as_str().unwrap()).file_name(),
        pdf.file_name()
    );
    assert_eq!(hit["kind"], "chunk");
    assert_eq!(hit["collection"], "work");
    assert_eq!(hit["pages"]["first"], 1);
    assert_eq!(hit["source"]["name"], "invoice.txt");
    assert!(hit["snippet"].as_str().unwrap().contains("[invoice]"));

    // Prefixes and phrases work; FTS operators in the text are just words.
    let text = |args: &[&str]| String::from_utf8(ok(run(dir, &index, args)).stdout).unwrap();
    assert!(text(&["search", "invo*"]).contains("invoice.txt.pdf p.1  chunk"));
    assert!(text(&["search", "\"from acme\""]).contains("[from Acme]"));
    assert!(text(&["search", "acme", "NOT", "OR"]).is_empty());
    assert!(text(&["search", "relevant", "--collection", "work"]).is_empty());
    assert!(text(&["search", "relevant"]).contains("other.txt"));

    let list = json_of(run(dir, &index, &["index", "list", "--json"]), "index");
    let docs = list["documents"].as_array().unwrap();
    assert_eq!(docs.len(), 2);
    assert!(docs.iter().all(|d| d["origin"] == "convert"));
}

#[test]
fn index_add_reads_existing_pdfs_and_remove_forgets_them() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let index = dir.join("index.sqlite");
    let unused = dir.join("unused.sqlite");
    let pdf = convert(dir, &unused, "notes.md", "Budget approved by Alice\n", &[]);
    assert!(
        !unused.exists(),
        "convert without --index must not create an index"
    );

    let pdf_arg = pdf.to_str().unwrap();
    let added = json_of(
        run(
            dir,
            &index,
            &["index", "add", pdf_arg, "--collection", "minutes", "--json"],
        ),
        "index",
    );
    assert_eq!(added["documents"][0]["origin"], "pdf");
    assert_eq!(added["documents"][0]["collection"], "minutes");
    assert_eq!(added["documents"][0]["entries"], 1);

    // Re-adding without --collection replaces the record but keeps its collection.
    let again = json_of(
        run(dir, &index, &["index", "add", pdf_arg, "--json"]),
        "index",
    );
    assert_eq!(again["documents"][0]["collection"], "minutes");

    let doc = json_of(
        run(
            dir,
            &index,
            &[
                "search",
                "alice budget",
                "--collection",
                "minutes",
                "--json",
            ],
        ),
        "search",
    );
    assert_eq!(doc["hits"].as_array().unwrap().len(), 1);
    assert_eq!(doc["hits"][0]["source"]["name"], "notes.md");

    ok(run(dir, &index, &["index", "remove", pdf_arg]));
    let doc = json_of(run(dir, &index, &["search", "alice", "--json"]), "search");
    assert!(doc["hits"].as_array().unwrap().is_empty());
    let missing = run(dir, &index, &["index", "remove", pdf_arg]);
    assert_eq!(missing.status.code(), Some(3));
}

#[test]
fn search_reports_usage_and_missing_index_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let index = dir.join("absent.sqlite");
    assert_eq!(run(dir, &index, &["search"]).status.code(), Some(2));
    let out = run(dir, &index, &["search", "anything"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no search index"));
    assert!(!index.exists(), "searching must not create an index");
    assert_eq!(
        run(dir, &index, &["search", "x", "--kind", "gender"])
            .status
            .code(),
        Some(2)
    );
    // --index-db and --collection on convert need --index.
    fs::write(dir.join("a.txt"), "a").unwrap();
    assert_eq!(
        run(dir, &index, &["convert", "a.txt", "--collection", "c"])
            .status
            .code(),
        Some(2)
    );
    // A file without an embedded manifest cannot be indexed.
    let not_pdf = dir.join("plain.txt");
    fs::write(&not_pdf, "not a pdf").unwrap();
    let out = run(dir, &index, &["index", "add", not_pdf.to_str().unwrap()]);
    assert_eq!(out.status.code(), Some(3));
}
