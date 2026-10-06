//! `anytopdf-plugin-sentiment`: an anytopdf runtime plugin (protocol v1) that
//! labels the sentiment (positive, negative, neutral) and tone (question,
//! complaint, urgent, and from an LLM also formal or informal) of transcript
//! segments, caption cues, OCR blocks and plain-text paragraphs.
//!
//! It registers as a unit enricher and appends `custom` annotations with an
//! `entity` attribute (`sentiment`, `tone`, `sentiment-overall`), so searching a
//! PDF for "negative" or "complaint" finds those moments. Labels describe what
//! the text says. They are never derived from faces or voices and never
//! attached to a person.

mod llm;
mod segments;
mod tone;
mod vader;

use anyhow::{Context, Result, bail};
use segments::{Origin, Segment};
use serde_json::{Map, Value, json};
use std::{env, fs, path::Path, process::ExitCode, time::Duration};

const PROTOCOL: u64 = 1;
const DEFAULT_THRESHOLD: f64 = 0.05;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match args.as_slice() {
        [flag] if flag == "--anytopdf-manifest" => {
            println!("{}", manifest());
            ExitCode::SUCCESS
        }
        [flag] if flag == "--version" => {
            println!("anytopdf-plugin-sentiment {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        [req_flag, request, resp_flag, response]
            if req_flag == "--anytopdf-request" && resp_flag == "--anytopdf-response" =>
        {
            let body = handle(Path::new(request), |name| env::var(name).ok())
                .unwrap_or_else(|e| {
                    json!({"protocol": PROTOCOL, "ok": false, "warnings": [], "error": format!("{e:#}")})
                });
            match write_atomic(Path::new(response), &body) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("anytopdf-plugin-sentiment: {e:#}");
                    ExitCode::FAILURE
                }
            }
        }
        _ => {
            eprintln!(
                "usage: anytopdf-plugin-sentiment --anytopdf-manifest\n       \
                 anytopdf-plugin-sentiment --anytopdf-request REQUEST --anytopdf-response RESPONSE"
            );
            ExitCode::from(2)
        }
    }
}

fn manifest() -> Value {
    json!({
        "protocol": PROTOCOL,
        "name": "sentiment",
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": [{
            "kind": "unit-enricher",
            "extensions": [],
            "mime_types": [],
            "priority": 50
        }]
    })
}

#[derive(Debug, Clone)]
enum Backend {
    Vader,
    Llm(llm::Llm),
}

/// Settings from `ANYTOPDF_SENTIMENT_*` environment variables (the
/// `[sentiment]` section of the layered configuration).
#[derive(Debug, Clone)]
struct Config {
    backend: Backend,
    threshold: f64,
    neutral: bool,
    tones: bool,
    from: Vec<Origin>,
}

fn flag(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

impl Config {
    fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<Self> {
        let get = |name: &str| {
            var(&format!("ANYTOPDF_SENTIMENT_{name}")).filter(|v| !v.trim().is_empty())
        };
        let llm_url = get("LLM_URL");
        let backend = match get("BACKEND").as_deref().map(str::trim) {
            None | Some("auto") if llm_url.is_none() => Backend::Vader,
            Some("vader") => Backend::Vader,
            None | Some("auto") | Some("llm") => {
                let url = llm_url
                    .context("ANYTOPDF_SENTIMENT_LLM_URL is required for the llm backend")?;
                let model = get("LLM_MODEL")
                    .context("ANYTOPDF_SENTIMENT_LLM_MODEL is required for the llm backend")?;
                let timeout = match get("LLM_TIMEOUT") {
                    Some(v) => v
                        .trim()
                        .parse::<u64>()
                        .ok()
                        .filter(|s| *s > 0)
                        .with_context(|| {
                            format!(
                                "ANYTOPDF_SENTIMENT_LLM_TIMEOUT must be whole seconds, got {v:?}"
                            )
                        })?,
                    None => 45,
                };
                Backend::Llm(llm::Llm {
                    url,
                    model,
                    api_key: get("LLM_API_KEY"),
                    timeout: Duration::from_secs(timeout),
                })
            }
            Some(other) => {
                bail!("ANYTOPDF_SENTIMENT_BACKEND must be auto, vader or llm, got {other:?}")
            }
        };
        let threshold = match get("THRESHOLD") {
            Some(v) => v
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|t| (0.0..1.0).contains(t))
                .with_context(|| {
                    format!(
                        "ANYTOPDF_SENTIMENT_THRESHOLD must be at least 0 and below 1, got {v:?}"
                    )
                })?,
            None => DEFAULT_THRESHOLD,
        };
        let boolean = |name: &str, default: bool| -> Result<bool> {
            match get(name) {
                Some(v) => flag(&v).with_context(|| {
                    format!("ANYTOPDF_SENTIMENT_{name} must be true or false, got {v:?}")
                }),
                None => Ok(default),
            }
        };
        let from = match get("FROM") {
            Some(v) => v
                .split(',')
                .filter(|s| !s.trim().is_empty())
                .map(|s| {
                    Origin::parse(s).with_context(|| {
                        format!(
                            "ANYTOPDF_SENTIMENT_FROM lists transcript, caption, ocr or text, got {s:?}"
                        )
                    })
                })
                .collect::<Result<Vec<_>>>()?,
            None => vec![Origin::Transcript, Origin::Caption, Origin::Ocr, Origin::Text],
        };
        Ok(Self {
            backend,
            threshold,
            neutral: boolean("NEUTRAL", false)?,
            tones: boolean("TONES", true)?,
            from,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Analysis {
    label: &'static str,
    /// Signed strength from -1 (negative) to 1 (positive).
    score: f64,
    confidence: f64,
    tones: Vec<&'static str>,
}

fn label_for(score: f64, threshold: f64) -> &'static str {
    if score >= threshold {
        "positive"
    } else if score <= -threshold {
        "negative"
    } else {
        "neutral"
    }
}

fn vader_analysis(text: &str, config: &Config) -> Analysis {
    let score = vader::compound(text);
    let label = label_for(score, config.threshold);
    Analysis {
        label,
        score,
        confidence: if label == "neutral" {
            1.0 - score.abs()
        } else {
            score.abs()
        },
        tones: if config.tones {
            tone::tags(text, label)
        } else {
            Vec::new()
        },
    }
}

/// Labels every segment with the configured backend. An LLM failure falls back
/// to the lexicon with a warning, so a stopped model server never costs the
/// whole conversion its sentiment.
fn analyze(
    segments: &[Segment],
    config: &Config,
    warnings: &mut Vec<String>,
) -> (Vec<Analysis>, String) {
    let lexicon = |s: &Segment| vader_analysis(&s.text, config);
    let Backend::Llm(llm) = &config.backend else {
        return (
            segments.iter().map(lexicon).collect(),
            "sentiment-vader".into(),
        );
    };
    let texts: Vec<&str> = segments.iter().map(|s| s.text.as_str()).collect();
    match llm.classify(&texts) {
        Ok(verdicts) => {
            let mut missing = 0;
            let analyses = segments
                .iter()
                .zip(verdicts)
                .map(|(segment, verdict)| match verdict {
                    Some(v) => Analysis {
                        label: v.label,
                        score: match v.label {
                            "positive" => v.confidence,
                            "negative" => -v.confidence,
                            _ => 0.0,
                        },
                        confidence: v.confidence,
                        tones: if config.tones { v.tones } else { Vec::new() },
                    },
                    None => {
                        missing += 1;
                        lexicon(segment)
                    }
                })
                .collect();
            if missing > 0 {
                warnings.push(format!(
                    "sentiment: the LLM gave no usable label for {missing} segment(s); used the VADER lexicon for those"
                ));
            }
            (analyses, "sentiment-llm".into())
        }
        Err(e) => {
            warnings.push(format!(
                "sentiment: LLM backend failed ({e:#}); used the VADER lexicon instead"
            ));
            (
                segments.iter().map(lexicon).collect(),
                "sentiment-vader".into(),
            )
        }
    }
}

fn annotation(
    entity: &str,
    text: &str,
    provider: &str,
    confidence: Option<f64>,
    segment: Option<&Segment>,
    mut attributes: Map<String, Value>,
) -> Value {
    attributes.insert("entity".into(), json!(entity));
    let (region, time_range) = match segment {
        Some(s) => {
            attributes.insert("from".into(), json!(s.from.name()));
            if let Some(p) = s.paragraph {
                attributes.insert("paragraph".into(), json!(p.to_string()));
            }
            (
                s.region
                    .as_ref()
                    .map(|r| json!({"x": r.x, "y": r.y, "width": r.width, "height": r.height})),
                s.time_range
                    .map(|(start, end)| json!({"start_seconds": start, "end_seconds": end})),
            )
        }
        None => (None, None),
    };
    json!({
        "kind": "custom",
        "text": text,
        "provider": provider,
        "confidence": confidence.map(|c| (c.clamp(0.0, 1.0) * 1000.0).round() / 1000.0),
        "region": region,
        "time_range": time_range,
        "attributes": attributes,
    })
}

/// The annotations for one unit: per-segment sentiment (neutral only when
/// asked for) and tone, then an overall line when the unit has several
/// segments.
fn annotate(
    segments: &[Segment],
    analyses: &[Analysis],
    provider: &str,
    config: &Config,
    model: Option<&str>,
) -> Vec<Value> {
    let base = || {
        let mut attributes = Map::new();
        if let Some(model) = model {
            attributes.insert("model".into(), json!(model));
        }
        attributes
    };
    let mut out = Vec::new();
    for (segment, analysis) in segments.iter().zip(analyses) {
        if analysis.label != "neutral" || config.neutral {
            let mut attributes = base();
            attributes.insert("label".into(), json!(analysis.label));
            attributes.insert("score".into(), json!(format!("{:.4}", analysis.score)));
            out.push(annotation(
                "sentiment",
                analysis.label,
                provider,
                Some(analysis.confidence),
                Some(segment),
                attributes,
            ));
        }
        for tone in &analysis.tones {
            out.push(annotation(
                "tone",
                tone,
                provider,
                None,
                Some(segment),
                base(),
            ));
        }
    }
    if segments.len() > 1 {
        // Longer segments weigh more, so one short "thanks!" does not outvote
        // a long complaint.
        let weight = |s: &Segment| s.text.split_whitespace().count().max(1) as f64;
        let total: f64 = segments.iter().map(weight).sum();
        let score = segments
            .iter()
            .zip(analyses)
            .map(|(s, a)| a.score * weight(s))
            .sum::<f64>()
            / total;
        let label = label_for(score, config.threshold);
        let count = |l: &str| analyses.iter().filter(|a| a.label == l).count();
        let mut attributes = base();
        attributes.insert("label".into(), json!(label));
        attributes.insert("score".into(), json!(format!("{score:.4}")));
        attributes.insert("segments".into(), json!(segments.len().to_string()));
        attributes.insert("positive".into(), json!(count("positive").to_string()));
        attributes.insert("negative".into(), json!(count("negative").to_string()));
        attributes.insert("neutral".into(), json!(count("neutral").to_string()));
        let times: Vec<(f64, f64)> = segments.iter().filter_map(|s| s.time_range).collect();
        let mut overall = annotation(
            "sentiment-overall",
            &format!("overall {label}"),
            provider,
            None,
            None,
            attributes,
        );
        if let (Some(start), Some(end)) = (
            times.iter().map(|t| t.0).reduce(f64::min),
            times.iter().map(|t| t.1).reduce(f64::max),
        ) {
            overall["time_range"] = json!({"start_seconds": start, "end_seconds": end});
        }
        out.push(overall);
    }
    out
}

fn handle(request_path: &Path, var: impl Fn(&str) -> Option<String>) -> Result<Value> {
    let raw = fs::read_to_string(request_path).context("read request")?;
    let request: Value = serde_json::from_str(&raw).context("parse request")?;
    if request["protocol"].as_u64() != Some(PROTOCOL) {
        bail!("unsupported protocol {}", request["protocol"]);
    }
    if request["operation"] != "unit-enrich" {
        bail!("unsupported operation {}", request["operation"]);
    }
    let unit = &request["unit"];
    if !unit.is_object() {
        bail!("unit-enrich request has no unit");
    }
    let mut response = json!({"protocol": PROTOCOL, "ok": true, "warnings": [], "annotations": []});
    let config = match Config::from_env(var) {
        Ok(config) => config,
        Err(e) => {
            response["warnings"] = json!([format!("sentiment: {e:#}; nothing labelled")]);
            return Ok(response);
        }
    };
    let segments = segments::collect(unit, &config.from);
    if segments.is_empty() {
        return Ok(response);
    }
    let mut warnings = Vec::new();
    let (analyses, provider) = analyze(&segments, &config, &mut warnings);
    let model = match (&config.backend, provider.as_str()) {
        (Backend::Llm(llm), "sentiment-llm") => Some(llm.model.as_str()),
        _ => None,
    };
    response["annotations"] = json!(annotate(&segments, &analyses, &provider, &config, model));
    response["warnings"] = json!(warnings);
    Ok(response)
}

fn write_atomic(path: &Path, body: &Value) -> Result<()> {
    let mut name = path
        .file_name()
        .context("response path has no file name")?
        .to_os_string();
    name.push(".tmp");
    let temporary = path.with_file_name(name);
    fs::write(&temporary, serde_json::to_vec(body)?).context("write response")?;
    fs::rename(&temporary, path).context("publish response")?;
    Ok(())
}

#[cfg(test)]
mod tests;
