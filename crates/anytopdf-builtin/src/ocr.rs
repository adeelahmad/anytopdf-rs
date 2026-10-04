use anyhow::{Context, Result, bail};
use anytopdf_core::*;
use image::GenericImageView;
use serde_json::Value;
use std::{path::Path, process::Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrMode {
    Auto,
    Vision,
    DocTr,
    Tesseract,
    Off,
}

impl std::str::FromStr for OcrMode {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "vision" => Ok(Self::Vision),
            "doctr" => Ok(Self::DocTr),
            "tesseract" => Ok(Self::Tesseract),
            "off" | "none" => Ok(Self::Off),
            _ => Err(format!("unknown OCR mode: {s}")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct OcrProviderStatus {
    pub name: &'static str,
    pub available: bool,
    pub detail: String,
}

pub struct OcrEnricher {
    mode: OcrMode,
    lang: String,
}

impl OcrEnricher {
    pub fn new(mode: OcrMode, lang: String) -> Self {
        Self { mode, lang }
    }

    pub fn status() -> Vec<OcrProviderStatus> {
        vec![
            OcrProviderStatus {
                name: "vision",
                available: vision_available(),
                detail: if cfg!(target_os = "macos") {
                    "Apple Vision (native Rust bindings)".into()
                } else {
                    "Apple Vision is macOS-only".into()
                },
            },
            OcrProviderStatus {
                name: "doctr",
                available: doctr_available(),
                detail: "Python docTR provider".into(),
            },
            OcrProviderStatus {
                name: "tesseract",
                available: which::which("tesseract").is_ok(),
                detail: "Tesseract CLI provider".into(),
            },
        ]
    }
}

impl Plugin for OcrEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "ocr-auto".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "unit-enricher".into(),
            extensions: vec![],
            mime_types: vec!["image/*".into()],
            priority: 100,
        }
    }
}

impl UnitEnricher for OcrEnricher {
    fn supports(&self, _graph: &DocumentGraph, unit: &Unit) -> bool {
        self.mode != OcrMode::Off && unit.kind == UnitKind::Visual && unit.visual_path.is_some()
    }

    fn enrich_unit(
        &self,
        _ctx: &JobContext,
        _graph: &DocumentGraph,
        unit: &mut Unit,
    ) -> Result<Vec<String>> {
        let path = unit
            .visual_path
            .as_ref()
            .context("OCR unit has no visual path")?;
        let mut warnings = Vec::new();

        let providers: Vec<OcrMode> = match self.mode {
            OcrMode::Auto => vec![OcrMode::Vision, OcrMode::DocTr, OcrMode::Tesseract],
            other => vec![other],
        };

        let mut errors = Vec::new();
        for (index, provider) in providers.into_iter().enumerate() {
            let result = match provider {
                OcrMode::Vision => vision_ocr(path, &self.lang),
                OcrMode::DocTr => doctr_ocr(path, &self.lang),
                OcrMode::Tesseract => tesseract_ocr(path, &self.lang),
                _ => continue,
            };

            match result {
                Ok(annotations) => {
                    if self.mode == OcrMode::Auto && index > 0 {
                        warnings.push(fallback_notice(provider, &errors));
                    }
                    unit.annotations.extend(annotations);
                    return Ok(warnings);
                }
                Err(e) => errors.push(format!("{provider:?}: {e:#}")),
            }
        }

        bail!("no OCR provider succeeded: {}", errors.join("; "))
    }
}

fn tesseract_ocr(path: &Path, lang: &str) -> Result<Vec<Annotation>> {
    let exe = which::which("tesseract").context("tesseract not found")?;
    let img = image::open(path).with_context(|| format!("open {}", path.display()))?;
    let (width, height) = img.dimensions();

    let out = Command::new(exe)
        .arg(path)
        .args(["stdout", "-l", lang, "tsv"])
        .bounded_output(std::time::Duration::from_secs(180))
        .context("run tesseract")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr));
    }

    let tsv = String::from_utf8(out.stdout).context("Tesseract returned non-UTF-8 TSV")?;
    parse_tesseract_tsv(&tsv, width, height)
}

fn parse_tesseract_tsv(tsv: &str, width: u32, height: u32) -> Result<Vec<Annotation>> {
    if width == 0 || height == 0 {
        bail!("OCR image dimensions must be positive");
    }
    let mut rows = tsv.lines();
    let headers: Vec<_> = rows
        .next()
        .context("empty Tesseract TSV")?
        .split('\t')
        .collect();

    let col = |name: &str| {
        headers
            .iter()
            .position(|h| *h == name)
            .ok_or_else(|| anyhow::anyhow!("missing tesseract TSV column {name}"))
    };

    let text_i = col("text")?;
    let conf_i = col("conf")?;
    let left_i = col("left")?;
    let top_i = col("top")?;
    let width_i = col("width")?;
    let height_i = col("height")?;

    let mut annotations = Vec::new();
    for row in rows {
        // TSV is not CSV: quote characters in recognized text are literal.
        let record: Vec<_> = row.splitn(headers.len(), '\t').collect();
        if record.len() != headers.len() {
            bail!("malformed Tesseract TSV row");
        }
        let text = record.get(text_i).copied().unwrap_or("").trim();
        if text.is_empty() {
            continue;
        }
        let conf: f32 = record
            .get(conf_i)
            .copied()
            .unwrap_or("-1")
            .parse()
            .unwrap_or(-1.0);
        if conf < 0.0 {
            continue;
        }
        let left: f32 = record
            .get(left_i)
            .copied()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);
        let top: f32 = record
            .get(top_i)
            .copied()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);
        let w: f32 = record
            .get(width_i)
            .copied()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);
        let h: f32 = record
            .get(height_i)
            .copied()
            .unwrap_or("0")
            .parse()
            .unwrap_or(0.0);

        annotations.push(Annotation {
            kind: AnnotationKind::Ocr,
            text: text.to_string(),
            provider: "tesseract".into(),
            confidence: Some((conf / 100.0).clamp(0.0, 1.0)),
            region: Some(
                Region {
                    x: left / width as f32,
                    y: top / height as f32,
                    width: w / width as f32,
                    height: h / height as f32,
                }
                .clamped(),
            ),
            time_range: None,
            attributes: Metadata::new(),
        });
    }
    Ok(annotations)
}

fn doctr_available() -> bool {
    let python = match which::which("python3").or_else(|_| which::which("python")) {
        Ok(v) => v,
        Err(_) => return false,
    };
    Command::new(python)
        .args(["-c", "import doctr"])
        .bounded_output(std::time::Duration::from_secs(10))
        .map(|out| out.status.success())
        .unwrap_or(false)
}

fn doctr_ocr(path: &Path, _lang: &str) -> Result<Vec<Annotation>> {
    let python = which::which("python3")
        .or_else(|_| which::which("python"))
        .context("python not found")?;

    // docTR output is normalized to the same top-left Region model.
    let script = r#"
import json, sys
from doctr.io import DocumentFile
from doctr.models import ocr_predictor
m = ocr_predictor(pretrained=True)
doc = DocumentFile.from_images(sys.argv[1])
res = m(doc)
out = []
for p in res.pages:
    for b in p.blocks:
        for l in b.lines:
            for w in l.words:
                (x0,y0),(x1,y1) = w.geometry
                out.append({
                    "text": w.value,
                    "confidence": float(w.confidence),
                    "x": float(x0), "y": float(y0),
                    "width": float(x1-x0), "height": float(y1-y0)
                })
print(json.dumps(out, ensure_ascii=False))
"#;

    let out = Command::new(python)
        .args(["-c", script])
        .arg(path)
        .bounded_output(std::time::Duration::from_secs(180))
        .context("run docTR")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr));
    }

    let values: Vec<Value> = serde_json::from_slice(&out.stdout)?;
    Ok(values
        .into_iter()
        .filter_map(|v| {
            Some(Annotation {
                kind: AnnotationKind::Ocr,
                text: v["text"].as_str()?.to_string(),
                provider: "doctr".into(),
                confidence: v["confidence"].as_f64().map(|x| x as f32),
                region: Some(
                    Region {
                        x: v["x"].as_f64()? as f32,
                        y: v["y"].as_f64()? as f32,
                        width: v["width"].as_f64()? as f32,
                        height: v["height"].as_f64()? as f32,
                    }
                    .clamped(),
                ),
                time_range: None,
                attributes: Metadata::new(),
            })
        })
        .collect())
}

#[cfg(all(target_os = "macos", feature = "apple-vision"))]
fn vision_available() -> bool {
    objc2::available!(macos = 10.15)
}

#[cfg(not(all(target_os = "macos", feature = "apple-vision")))]
fn vision_available() -> bool {
    false
}

#[cfg(not(all(target_os = "macos", feature = "apple-vision")))]
fn vision_ocr(_path: &Path, _lang: &str) -> Result<Vec<Annotation>> {
    bail!("Apple Vision OCR unavailable in this build")
}

#[cfg(all(target_os = "macos", feature = "apple-vision"))]
fn vision_ocr(path: &Path, lang: &str) -> Result<Vec<Annotation>> {
    use objc2::{AnyThread, runtime::AnyObject};
    use objc2_foundation::{NSArray, NSData, NSDictionary, NSString};
    use objc2_vision::{
        VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
        VNRequestTextRecognitionLevel,
    };

    if !vision_available() {
        bail!("Apple Vision text recognition requires macOS 10.15 or later");
    }
    let bytes = std::fs::read(path)?;
    let data = NSData::with_bytes(&bytes);
    let options = NSDictionary::<VNImageOption, AnyObject>::new();
    let handler = VNImageRequestHandler::initWithData_options(
        VNImageRequestHandler::alloc(),
        &data,
        &options,
    );

    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);
    request.setUsesLanguageCorrection(true);
    if objc2::available!(macos = 13.0) {
        request.setAutomaticallyDetectsLanguage(true);
    }

    let normalized_language = lang.to_ascii_lowercase();
    let mapped = match normalized_language.as_str() {
        "eng" | "en" => "en-US",
        "fra" => "fr-FR",
        "deu" => "de-DE",
        "spa" => "es-ES",
        "ita" => "it-IT",
        other => other,
    };
    let language = NSString::from_str(mapped);
    let languages = NSArray::from_retained_slice(&[language]);
    request.setRecognitionLanguages(&languages);

    let request_ref: &VNRequest = request.as_ref();
    let requests = NSArray::from_slice(&[request_ref]);
    handler
        .performRequests_error(&requests)
        .map_err(|e| anyhow::anyhow!("Vision request failed: {e:?}"))?;

    let mut out = Vec::new();
    if let Some(results) = request.results() {
        for obs in results.iter() {
            let candidates = obs.topCandidates(1);
            let Some(candidate) = candidates.iter().next() else {
                continue;
            };
            let text = candidate.string().to_string();
            let bbox = unsafe { obs.boundingBox() };
            out.push(Annotation {
                kind: AnnotationKind::Ocr,
                text,
                provider: "apple-vision".into(),
                confidence: Some(candidate.confidence()),
                region: Some(
                    Region {
                        x: bbox.origin.x as f32,
                        y: (1.0 - bbox.origin.y - bbox.size.height) as f32,
                        width: bbox.size.width as f32,
                        height: bbox.size.height as f32,
                    }
                    .clamped(),
                ),
                time_range: None,
                attributes: Metadata::new(),
            });
        }
    }
    Ok(out)
}

fn fallback_notice(provider: OcrMode, errors: &[String]) -> String {
    Diagnostic::new(
        DiagnosticCode::OcrFallback,
        format!(
            "OCR fallback selected {provider:?}; earlier providers unavailable: {}",
            errors.join("; ")
        ),
    )
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_fallback_notice_is_informational() {
        let d = Diagnostic::from_wire(&fallback_notice(
            OcrMode::Tesseract,
            &["Vision: unavailable".into()],
        ));
        assert_eq!(d.code, DiagnosticCode::OcrFallback);
        assert_eq!(d.severity, Severity::Info);
        assert!(d.message.contains("Tesseract"), "{}", d.message);
        assert!(d.message.contains("Vision: unavailable"), "{}", d.message);
    }

    #[test]
    fn tesseract_preserves_quotes_and_tabs_in_text() {
        let tsv = "left\ttop\twidth\theight\tconf\ttext\n10\t20\t30\t40\t95.5\t\"quoted\"\ttext\n";
        let annotations = parse_tesseract_tsv(tsv, 100, 200).unwrap();
        assert_eq!(annotations[0].text, "\"quoted\"\ttext");
        assert_eq!(annotations[0].region.unwrap().y, 0.1);
        assert_eq!(annotations[0].confidence, Some(0.955));
    }

    #[test]
    fn tesseract_rejects_malformed_output() {
        assert!(parse_tesseract_tsv("bad headers", 100, 100).is_err());
        assert!(parse_tesseract_tsv("", 100, 100).is_err());
    }
}
