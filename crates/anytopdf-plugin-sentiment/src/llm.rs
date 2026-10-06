//! Sentiment and tone from a local OpenAI-compatible chat endpoint (Ollama,
//! llama.cpp `llama-server`, LM Studio, vLLM). This is the multilingual backend:
//! the VADER lexicon only knows English.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::Duration;

/// Segments sent per request, so one long transcript does not become one
/// enormous prompt.
pub const BATCH: usize = 24;
/// Characters of a segment sent to the model.
const MAX_SEGMENT_CHARS: usize = 2000;

pub const TONES: &[&str] = &["question", "complaint", "urgent", "formal", "informal"];

const SYSTEM: &str = "You label the sentiment and tone of text segments taken from \
transcripts, subtitles, OCR and documents. Judge only what the text says. Never guess \
a speaker's or writer's emotional state, identity, age, gender or other personal \
traits. For every input segment return one result with: \"i\" (the segment's index), \
\"label\" (\"positive\", \"negative\" or \"neutral\"), \"confidence\" (0 to 1) and \
\"tones\" (zero or more of \"question\", \"complaint\", \"urgent\", \"formal\", \
\"informal\"). Reply with JSON only: {\"results\": [...]}.";

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub label: &'static str,
    pub confidence: f64,
    pub tones: Vec<&'static str>,
}

#[derive(Debug, Clone)]
pub struct Llm {
    pub url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub timeout: Duration,
}

impl Llm {
    fn endpoint(&self) -> String {
        let base = self.url.trim_end_matches('/');
        if base.ends_with("/chat/completions") {
            base.to_string()
        } else {
            format!("{base}/chat/completions")
        }
    }

    /// One verdict per text, `None` where the model's answer had no usable
    /// result for that segment.
    pub fn classify(&self, texts: &[&str]) -> Result<Vec<Option<Verdict>>> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .http_status_as_error(false)
            .user_agent(concat!(
                "anytopdf-plugin-sentiment/",
                env!("CARGO_PKG_VERSION")
            ))
            .build()
            .into();
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(BATCH) {
            out.extend(self.classify_batch(&agent, batch)?);
        }
        Ok(out)
    }

    fn classify_batch(&self, agent: &ureq::Agent, texts: &[&str]) -> Result<Vec<Option<Verdict>>> {
        let segments: Vec<Value> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| json!({"i": i, "text": t.chars().take(MAX_SEGMENT_CHARS).collect::<String>()}))
            .collect();
        let body = json!({
            "model": self.model,
            "temperature": 0,
            "response_format": {"type": "json_object"},
            "messages": [
                {"role": "system", "content": SYSTEM},
                {"role": "user", "content": json!({"segments": segments}).to_string()}
            ]
        });
        let mut request = agent
            .post(&self.endpoint())
            .header("Content-Type", "application/json");
        if let Some(key) = &self.api_key {
            request = request.header("Authorization", &format!("Bearer {key}"));
        }
        let mut response = request
            .send(body.to_string())
            .with_context(|| format!("request {}", self.endpoint()))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .context("read LLM response")?;
        if !(200..300).contains(&status) {
            let detail: String = text.chars().take(200).collect();
            bail!("LLM endpoint answered HTTP {status}: {}", detail.trim());
        }
        let reply: Value = serde_json::from_str(&text).context("LLM response is not JSON")?;
        let content = reply["choices"][0]["message"]["content"]
            .as_str()
            .context("LLM response has no message content")?;
        parse(content, texts.len())
    }
}

/// Reads `{"results": [...]}` (or a bare array), tolerating a Markdown code
/// fence around it.
pub fn parse(content: &str, expected: usize) -> Result<Vec<Option<Verdict>>> {
    let trimmed = content.trim();
    let unfenced = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|s| s.trim_end().strip_suffix("```"))
        .unwrap_or(trimmed);
    let value: Value = serde_json::from_str(unfenced.trim()).context("model reply is not JSON")?;
    let results = value
        .get("results")
        .unwrap_or(&value)
        .as_array()
        .context("model reply has no results array")?;
    let mut out = vec![None; expected];
    for (position, item) in results.iter().enumerate() {
        let index = item["i"].as_u64().map(|i| i as usize).unwrap_or(position);
        let label = match item["label"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("positive") => "positive",
            Some("negative") => "negative",
            Some("neutral") => "neutral",
            _ => continue,
        };
        let confidence = item["confidence"]
            .as_f64()
            .or_else(|| item["score"].as_f64())
            .filter(|c| c.is_finite())
            .map(|c| c.clamp(0.0, 1.0))
            .unwrap_or(0.5);
        let tones = item["tones"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| {
                let t = t.as_str()?.to_ascii_lowercase();
                TONES.iter().find(|known| **known == t).copied()
            })
            .collect();
        if let Some(slot) = out.get_mut(index) {
            *slot = Some(Verdict {
                label,
                confidence,
                tones,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_are_read_with_or_without_a_code_fence() {
        let reply = "```json\n{\"results\": [{\"i\": 1, \"label\": \"Negative\", \"confidence\": 0.9, \
                     \"tones\": [\"complaint\", \"sarcastic\", \"urgent\"]}, \
                     {\"i\": 0, \"label\": \"positive\", \"score\": 1.7}]}\n```";
        let got = parse(reply, 3).unwrap();
        assert_eq!(
            got[0],
            Some(Verdict {
                label: "positive",
                confidence: 1.0,
                tones: vec![]
            })
        );
        assert_eq!(
            got[1],
            Some(Verdict {
                label: "negative",
                confidence: 0.9,
                tones: vec!["complaint", "urgent"]
            })
        );
        assert_eq!(got[2], None);
    }

    #[test]
    fn unknown_labels_and_out_of_range_indexes_are_ignored() {
        let got = parse(
            r#"[{"label": "angry"}, {"i": 9, "label": "neutral"}, {"label": "neutral"}]"#,
            3,
        )
        .unwrap();
        assert_eq!(got[0], None);
        assert_eq!(got[2].as_ref().unwrap().label, "neutral");
        assert!(parse("not json", 1).is_err());
    }

    #[test]
    fn endpoint_accepts_a_base_url_or_the_full_path() {
        let llm = |url: &str| Llm {
            url: url.into(),
            model: "m".into(),
            api_key: None,
            timeout: Duration::from_secs(1),
        };
        assert_eq!(
            llm("http://127.0.0.1:11434/v1/").endpoint(),
            "http://127.0.0.1:11434/v1/chat/completions"
        );
        assert_eq!(
            llm("http://h/v1/chat/completions").endpoint(),
            "http://h/v1/chat/completions"
        );
    }
}
