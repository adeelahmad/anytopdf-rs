use anyhow::{Context, Result, bail};
use anytopdf_core::*;
use regex::Regex;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// Unit metadata key marking pages whose searchable text came from the
/// document itself; OCR skips such pages.
pub const TEXT_LAYER_KEY: &str = "text-layer";
pub const TEXT_LAYER_NATIVE: &str = "native";

const PROVIDER: &str = "libreoffice";
const TEXT_PROVIDER: &str = "pdftotext";
const CONVERT_TIMEOUT: Duration = Duration::from_secs(300);
/// Matches the default renderer DPI so pages keep their printed size.
const RASTER_DPI: u32 = 144;

const EXTENSIONS: [&str; 20] = [
    "doc", "docx", "docm", "dot", "dotx", "odt", "ott", "rtf", "wpd", "xls", "xlsx", "xlsm", "ods",
    "ots", "ppt", "pptx", "pptm", "pps", "ppsx", "odp",
];

const MIME_TYPES: [&str; 13] = [
    "application/msword",
    "application/rtf",
    "text/rtf",
    "application/vnd.ms-excel",
    "application/vnd.ms-powerpoint",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    "application/vnd.oasis.opendocument.text",
    "application/vnd.oasis.opendocument.spreadsheet",
    "application/vnd.oasis.opendocument.presentation",
    "application/vnd.ms-word.document.macroEnabled.12",
    "application/vnd.ms-excel.sheet.macroEnabled.12",
];

/// Imports word-processing, spreadsheet and presentation files by converting
/// them to PDF with headless LibreOffice, rasterizing each page with Poppler
/// and attaching the document's own positioned text.
pub struct OfficeImporter;

impl Plugin for OfficeImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "office".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: EXTENSIONS.into_iter().map(str::to_string).collect(),
            mime_types: MIME_TYPES.into_iter().map(str::to_string).collect(),
            priority: 60,
        }
    }
}

impl Importer for OfficeImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        if source
            .detected_type
            .as_deref()
            .is_some_and(|m| MIME_TYPES.contains(&m))
        {
            return ProbeScore::MAGIC;
        }
        let ext = source
            .path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if EXTENSIONS.contains(&ext.as_str()) {
            ProbeScore::EXTENSION
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, ctx: &JobContext, source: SourceRecord) -> Result<ImportOutcome> {
        let soffice =
            soffice_path().context("office input requires LibreOffice (soffice) on PATH")?;
        let pdftoppm = which::which("pdftoppm")
            .context("office input requires pdftoppm (Poppler) to rasterize pages")?;
        let root = ctx.workspace.join(format!("office-{}", source.id));
        let pdf = convert_to_pdf(&soffice, &source.path, &root)?;
        let pages = rasterize(&pdftoppm, &pdf, &root.join("pages"))?;

        let mut warnings = Vec::new();
        let text = match which::which("pdftotext") {
            Ok(pdftotext) => match page_text(&pdftotext, &pdf, &root) {
                Ok(text) => text,
                Err(e) => {
                    warnings.push(
                        Diagnostic::new(
                            DiagnosticCode::ProviderFailed,
                            format!("pdftotext text layer: {e:#}; pages fall back to OCR"),
                        )
                        .to_string(),
                    );
                    Vec::new()
                }
            },
            Err(_) => {
                warnings.push(
                    Diagnostic::new(
                        DiagnosticCode::ProviderMissing,
                        "pdftotext not found; office pages fall back to OCR",
                    )
                    .to_string(),
                );
                Vec::new()
            }
        };

        let units = pages
            .into_iter()
            .enumerate()
            .map(|(index, path)| {
                let mut unit = Unit::visual(source.id, path);
                unit.metadata
                    .insert("office.page".into(), (index + 1).to_string());
                unit.metadata
                    .insert("office.converter".into(), PROVIDER.into());
                if let Some(lines) = text.get(index).filter(|lines| !lines.is_empty()) {
                    unit.metadata
                        .insert(TEXT_LAYER_KEY.into(), TEXT_LAYER_NATIVE.into());
                    unit.annotations.extend(lines.iter().map(line_annotation));
                }
                unit
            })
            .collect();
        Ok(ImportOutcome {
            source,
            units,
            warnings,
        })
    }
}

/// Finds LibreOffice on PATH, then in its default macOS and Windows locations.
pub fn soffice_path() -> Option<PathBuf> {
    if let Some(path) = ["soffice", "libreoffice"]
        .into_iter()
        .find_map(|name| which::which(name).ok())
    {
        return Some(path);
    }
    let candidates: &[&str] = if cfg!(target_os = "macos") {
        &["/Applications/LibreOffice.app/Contents/MacOS/soffice"]
    } else if cfg!(windows) {
        &[
            r"C:\Program Files\LibreOffice\program\soffice.exe",
            r"C:\Program Files (x86)\LibreOffice\program\soffice.exe",
        ]
    } else {
        &[]
    };
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

fn convert_to_pdf(soffice: &Path, input: &Path, root: &Path) -> Result<PathBuf> {
    // A private copy keeps LibreOffice lock files away from the original and
    // gives the output a predictable name.
    let staged_dir = root.join("input");
    let out_dir = root.join("pdf");
    fs::create_dir_all(&staged_dir)?;
    fs::create_dir_all(&out_dir)?;
    let ext = input
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("bin")
        .to_ascii_lowercase();
    let staged = staged_dir.join(format!("document.{ext}"));
    fs::copy(input, &staged).with_context(|| format!("stage {}", input.display()))?;

    // A job-local profile avoids clashing with a running LibreOffice instance.
    let profile = file_url(&root.join("profile"));
    let output = Command::new(soffice)
        .arg(format!("-env:UserInstallation={profile}"))
        .args([
            "--headless",
            "--invisible",
            "--nodefault",
            "--nolockcheck",
            "--nologo",
            "--norestore",
            "--convert-to",
            "pdf",
            "--outdir",
        ])
        .arg(plain_path(&out_dir))
        .arg(plain_path(&staged))
        .bounded_output(CONVERT_TIMEOUT)
        .context("run LibreOffice conversion")?;
    let pdf = out_dir.join("document.pdf");
    if !output.status.success() || !pdf.is_file() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let reason = stderr
            .lines()
            .chain(stdout.lines())
            .rfind(|line| line.to_ascii_lowercase().contains("error"))
            .unwrap_or("no PDF was produced");
        bail!(
            "LibreOffice could not convert the document: {}",
            reason.trim()
        );
    }
    Ok(pdf)
}

pub(crate) fn rasterize(pdftoppm: &Path, pdf: &Path, dir: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dir)?;
    let output = Command::new(pdftoppm)
        .args(["-r", &RASTER_DPI.to_string(), "-png"])
        .arg(plain_path(pdf))
        .arg(plain_path(&dir.join("page")))
        .bounded_output(CONVERT_TIMEOUT)
        .context("run pdftoppm")?;
    if !output.status.success() {
        bail!(
            "pdftoppm failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    // pdftoppm zero-pads page numbers to a common width, so names sort in page order.
    let mut pages: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("png"))
        .collect();
    pages.sort();
    if pages.is_empty() {
        bail!("the converted document has no pages");
    }
    Ok(pages)
}

#[derive(Debug, Clone)]
pub(crate) struct TextLine {
    pub text: String,
    pub region: Region,
}

pub(crate) fn page_text(pdftotext: &Path, pdf: &Path, root: &Path) -> Result<Vec<Vec<TextLine>>> {
    let html = root.join("text.html");
    let output = Command::new(pdftotext)
        .args(["-bbox-layout", "-enc", "UTF-8"])
        .arg(plain_path(pdf))
        .arg(plain_path(&html))
        .bounded_output(CONVERT_TIMEOUT)
        .context("run pdftotext")?;
    if !output.status.success() {
        bail!(
            "pdftotext failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    parse_bbox_layout(&fs::read_to_string(&html)?)
}

/// Parses `pdftotext -bbox-layout` XHTML into per-page lines with regions
/// normalized to the page size.
pub(crate) fn parse_bbox_layout(html: &str) -> Result<Vec<Vec<TextLine>>> {
    let page_re = Regex::new(r#"<page\s+width="([0-9.]+)"\s+height="([0-9.]+)""#)?;
    let line_re = Regex::new(
        r#"(?s)<line\s+xMin="([0-9.]+)"\s+yMin="([0-9.]+)"\s+xMax="([0-9.]+)"\s+yMax="([0-9.]+)"\s*>(.*?)</line>"#,
    )?;
    let word_re = Regex::new(r"(?s)<word[^>]*>(.*?)</word>")?;
    let starts: Vec<_> = page_re.captures_iter(html).collect();
    let mut pages = Vec::with_capacity(starts.len());
    for (index, page) in starts.iter().enumerate() {
        let width: f32 = page[1].parse()?;
        let height: f32 = page[2].parse()?;
        if width <= 0.0 || height <= 0.0 {
            bail!("pdftotext reported an empty page size");
        }
        let begin = page.get(0).map_or(0, |m| m.end());
        let end = starts
            .get(index + 1)
            .and_then(|next| next.get(0))
            .map_or(html.len(), |m| m.start());
        let mut lines = Vec::new();
        for line in line_re.captures_iter(&html[begin..end]) {
            let text = word_re
                .captures_iter(&line[5])
                .map(|w| unescape(w[1].trim()))
                .filter(|w| !w.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if text.is_empty() {
                continue;
            }
            let coords: Vec<f32> = (1..=4)
                .map(|i| line[i].parse::<f32>())
                .collect::<Result<_, _>>()?;
            let region = Region {
                x: coords[0] / width,
                y: coords[1] / height,
                width: (coords[2] - coords[0]) / width,
                height: (coords[3] - coords[1]) / height,
            }
            .clamped();
            lines.push(TextLine { text, region });
        }
        pages.push(lines);
    }
    Ok(pages)
}

pub(crate) fn line_annotation(line: &TextLine) -> Annotation {
    let mut annotation = Annotation::text(AnnotationKind::Ocr, TEXT_PROVIDER, line.text.clone());
    annotation.region = Some(line.region);
    annotation.confidence = Some(1.0);
    annotation
        .attributes
        .insert("text_source".into(), TEXT_LAYER_NATIVE.into());
    annotation
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Drops the Windows verbatim prefix that canonical paths carry, which
/// LibreOffice and Poppler do not understand.
fn plain_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

pub(crate) fn file_url(path: &Path) -> String {
    let text = plain_path(path).to_string_lossy().replace('\\', "/");
    let mut url = String::from("file://");
    if !text.starts_with('/') {
        url.push('/');
    }
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYOUT: &str = r#"<!DOCTYPE html><html><body><doc>
  <page width="200.000000" height="100.000000">
    <flow><block xMin="10" yMin="10" xMax="110" yMax="20">
      <line xMin="10.000000" yMin="10.000000" xMax="110.000000" yMax="20.000000">
        <word xMin="10" yMin="10" xMax="50" yMax="20">Invoice</word>
        <word xMin="55" yMin="10" xMax="110" yMax="20">&amp;&lt;42&gt;</word>
      </line>
    </block></flow>
  </page>
  <page width="200.000000" height="100.000000">
  </page>
</doc></body></html>"#;

    #[test]
    fn bbox_layout_lines_become_normalized_regions_per_page() {
        let pages = parse_bbox_layout(LAYOUT).unwrap();
        assert_eq!(pages.len(), 2);
        assert!(pages[1].is_empty());
        let line = &pages[0][0];
        assert_eq!(line.text, "Invoice &<42>");
        assert!((line.region.x - 0.05).abs() < 1e-6);
        assert!((line.region.y - 0.10).abs() < 1e-6);
        assert!((line.region.width - 0.50).abs() < 1e-6);
        assert!((line.region.height - 0.10).abs() < 1e-6);
    }

    #[test]
    fn probe_prefers_office_magic_over_extension() {
        let mut source = SourceRecord::new(PathBuf::from("/data/report.bin"));
        assert_eq!(OfficeImporter.probe(&source), ProbeScore::NONE);
        source.detected_type =
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document".into());
        assert_eq!(OfficeImporter.probe(&source), ProbeScore::MAGIC);
        let slides = SourceRecord::new(PathBuf::from("/data/Deck.PPTX"));
        assert_eq!(OfficeImporter.probe(&slides), ProbeScore::EXTENSION);
    }

    #[test]
    fn file_urls_escape_spaces_and_windows_drives() {
        assert_eq!(
            file_url(Path::new("/tmp/job one/profile")),
            "file:///tmp/job%20one/profile"
        );
        assert_eq!(
            file_url(Path::new(r"\\?\C:\Users\me\job\profile")),
            "file:///C:/Users/me/job/profile"
        );
    }

    #[test]
    fn missing_libreoffice_is_a_clear_import_error() {
        if soffice_path().is_some() && which::which("pdftoppm").is_ok() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memo.docx");
        fs::write(&path, b"not really a document").unwrap();
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let error = OfficeImporter
            .import(&ctx, SourceRecord::new(path))
            .unwrap_err();
        let message = format!("{error:#}");
        assert!(
            message.contains("soffice") || message.contains("pdftoppm"),
            "{message}"
        );
    }

    /// True only when this host can really convert a document. LibreOffice
    /// can be installed without its Writer component, which fails every
    /// conversion, so presence alone is not enough. Setting
    /// `ANYTOPDF_REQUIRE_OFFICE` turns a host that cannot convert into a
    /// failure instead of a skip (Linux CI sets it).
    fn office_converts() -> bool {
        let converts = soffice_path().is_some_and(|soffice| {
            if which::which("pdftoppm").is_err() || which::which("pdftotext").is_err() {
                return false;
            }
            let dir = tempfile::tempdir().unwrap();
            let probe = dir.path().join("probe.txt");
            fs::write(&probe, "probe").unwrap();
            convert_to_pdf(&soffice, &probe, dir.path()).is_ok()
        });
        let required = std::env::var_os("ANYTOPDF_REQUIRE_OFFICE").is_some_and(|v| !v.is_empty());
        assert!(
            converts || !required,
            "ANYTOPDF_REQUIRE_OFFICE is set but LibreOffice and Poppler cannot convert a document here"
        );
        converts
    }

    #[test]
    fn rtf_pages_carry_positioned_native_text_when_libreoffice_is_installed() {
        if !office_converts() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memo.rtf");
        fs::write(
            &path,
            "{\\rtf1\\ansi Invoice 42 paid\\par\\page Second page\\par}",
        )
        .unwrap();
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let outcome = OfficeImporter
            .import(&ctx, SourceRecord::new(path))
            .unwrap();
        assert_eq!(outcome.units.len(), 2, "{:?}", outcome.warnings);
        let first = &outcome.units[0];
        assert!(first.visual_path.as_ref().unwrap().starts_with(dir.path()));
        assert_eq!(
            first.metadata.get(TEXT_LAYER_KEY).map(String::as_str),
            Some(TEXT_LAYER_NATIVE)
        );
        let line = &first.annotations[0];
        assert_eq!(line.text, "Invoice 42 paid");
        assert!(line.region.is_some());
        assert_eq!(outcome.units[1].metadata["office.page"], "2");
    }
}
