use std::{
    fs,
    path::{Path, PathBuf},
    process::Output,
};

#[path = "common/process.rs"]
mod process;
#[path = "common/stdout_json.rs"]
mod stdout_json;
use process::command;
use stdout_json::single_document;

// printpdf's bundled Helvetica subset is a real TrueType font on every CI host.
fn fixture_font(dir: &Path) -> PathBuf {
    let path = dir.join("fixture.ttf");
    fs::write(
        &path,
        printpdf::BuiltinFont::Helvetica.get_subset_font().bytes,
    )
    .unwrap();
    path
}

fn convert_pdfa(dir: &Path, font: &Path, name: &str) -> (PathBuf, Output) {
    let notes = dir.join("notes.txt");
    fs::write(&notes, "PdfA fixture line\nsecond line\n").unwrap();
    let pdf = dir.join(name);
    let out = command()
        .env("SOURCE_DATE_EPOCH", "1700000000")
        .env("ANYTOPDF_FONT", font)
        .arg("convert")
        .arg(&notes)
        .args(["--renderer", "pdfa", "--ocr", "off", "-o"])
        .arg(&pdf)
        .output()
        .unwrap();
    (pdf, out)
}

#[test]
fn pdfa_renderer_writes_reproducible_pdfa3_with_one_set_of_attachments() {
    let dir = tempfile::tempdir().unwrap();
    let font = fixture_font(dir.path());
    let (pdf, out) = convert_pdfa(dir.path(), &font, "a.pdf");
    assert!(
        out.status.success(),
        "convert failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = fs::read(&pdf).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.contains("<pdfaid:part>3</pdfaid:part>"),
        "no PDF/A-3 XMP"
    );
    // The CLI must keep the renderer's associated files instead of adding its own copies.
    let specs = regex::Regex::new(r"/Type\s*/Filespec\b").unwrap();
    assert_eq!(specs.find_iter(&text).count(), 2, "attachments re-embedded");

    let (again, _) = convert_pdfa(dir.path(), &font, "b.pdf");
    assert!(
        bytes == fs::read(again).unwrap(),
        "pdfa output is not reproducible"
    );

    let out = command()
        .args(["extract", "--json"])
        .arg(&pdf)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "extract failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = single_document(&out.stdout);
    assert_eq!(doc["origin"], "embedded");
    assert_eq!(doc["warnings"].as_array().map(Vec::len), Some(0));
    assert_eq!(doc["manifest"]["schema_version"], "anytopdf.manifest/1");
    let chunk = &doc["chunks"]["chunks"][0];
    assert!(
        chunk["text"]
            .as_str()
            .unwrap()
            .contains("PdfA fixture line"),
        "{chunk}"
    );
}

#[test]
fn pdfa_renderer_without_a_usable_font_fails_without_output() {
    let dir = tempfile::tempdir().unwrap();
    let (pdf, out) = convert_pdfa(dir.path(), &dir.path().join("missing.ttf"), "a.pdf");
    assert!(!out.status.success(), "convert unexpectedly succeeded");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("missing.ttf"),
        "stderr does not name the font: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!pdf.exists(), "a failed render published output");
}
