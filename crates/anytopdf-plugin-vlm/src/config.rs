//! Configuration read from `ANYTOPDF_VLM_*` and `ANYTOPDF_LLM_*` variables.

use anyhow::{Result, bail};
use std::time::Duration;

/// The default prompts mirror the original video-analysis script: a caption,
/// an open question, and the activities in the frame.
pub const DEFAULT_PROMPTS: &[(&str, &str)] = &[
    ("caption", "Describe this image in one or two sentences."),
    ("query", "What do you see in this image?"),
    ("activity", "Describe the activities within the image."),
];

pub const DEFAULT_CATEGORIES: &str =
    "news,entertainment,education,sports,music,gaming,tutorial,meeting,documentary,other";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// `POST {url}/chat/completions` (Ollama, llama.cpp, LM Studio, vLLM).
    OpenAi,
    /// The Moondream API: `POST {url}/caption`, `/query`, `/detect`
    /// (Moondream Station, the bundled Moondream2 server, Moondream Cloud).
    Moondream,
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub engine: Engine,
    pub url: String,
    pub model: String,
    pub api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// The caption type recorded as `attributes.caption_type`.
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub vision: Endpoint,
    /// Text endpoint for summaries and categories; `None` disables them.
    pub text: Option<Endpoint>,
    pub prompts: Vec<Prompt>,
    pub detect: Vec<String>,
    pub categories: Vec<String>,
    /// Time budget for one plugin invocation (one keyframe, or all summaries).
    pub budget: Duration,
    pub max_side: u32,
}

impl Config {
    /// `Ok(None)` when `ANYTOPDF_VLM_URL` is unset: the plugin stays inert.
    pub fn from_env() -> Result<Option<Config>> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Option<Config>> {
        let var = |name: &str| {
            get(name)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let Some(url) = var("ANYTOPDF_VLM_URL") else {
            return Ok(None);
        };
        let engine = match var("ANYTOPDF_VLM_ENGINE").as_deref() {
            None | Some("openai") => Engine::OpenAi,
            Some("moondream") => Engine::Moondream,
            Some(other) => bail!("ANYTOPDF_VLM_ENGINE must be openai or moondream, not {other}"),
        };
        let vision = Endpoint {
            engine,
            url: endpoint_url(&url, "ANYTOPDF_VLM_URL")?,
            model: var("ANYTOPDF_VLM_MODEL").unwrap_or_else(|| "moondream".into()),
            api_key: var("ANYTOPDF_VLM_API_KEY"),
        };
        let summaries = !matches!(
            var("ANYTOPDF_VLM_SUMMARY").as_deref(),
            Some("off" | "0" | "false" | "no")
        );
        let text = match var("ANYTOPDF_LLM_URL") {
            _ if !summaries => None,
            Some(url) => Some(Endpoint {
                engine: Engine::OpenAi,
                url: endpoint_url(&url, "ANYTOPDF_LLM_URL")?,
                model: var("ANYTOPDF_LLM_MODEL").unwrap_or_else(|| vision.model.clone()),
                api_key: var("ANYTOPDF_LLM_API_KEY"),
            }),
            // The Moondream API has no text-only endpoint.
            None if engine == Engine::Moondream => None,
            None => Some(Endpoint {
                model: var("ANYTOPDF_LLM_MODEL").unwrap_or_else(|| vision.model.clone()),
                ..vision.clone()
            }),
        };
        let prompts = match var("ANYTOPDF_VLM_PROMPTS") {
            Some(list) => parse_prompts(&list)?,
            None => DEFAULT_PROMPTS
                .iter()
                .map(|(kind, text)| Prompt {
                    kind: (*kind).into(),
                    text: (*text).into(),
                })
                .collect(),
        };
        let budget = match var("ANYTOPDF_VLM_TIMEOUT") {
            None => 50.0,
            Some(v) => match v.parse::<f64>() {
                Ok(s) if s.is_finite() && s > 0.0 => s,
                _ => bail!("ANYTOPDF_VLM_TIMEOUT must be a positive number of seconds, not {v}"),
            },
        };
        let max_side = match var("ANYTOPDF_VLM_MAX_SIDE") {
            None => 768,
            Some(v) => match v.parse::<u32>() {
                Ok(n) if n >= 32 => n,
                _ => bail!("ANYTOPDF_VLM_MAX_SIDE must be a pixel size of at least 32, not {v}"),
            },
        };
        Ok(Some(Config {
            vision,
            text,
            prompts,
            detect: list(var("ANYTOPDF_VLM_DETECT").as_deref().unwrap_or("")),
            categories: list(
                var("ANYTOPDF_VLM_CATEGORIES")
                    .as_deref()
                    .unwrap_or(DEFAULT_CATEGORIES),
            ),
            budget: Duration::from_secs_f64(budget),
            max_side,
        }))
    }
}

fn endpoint_url(url: &str, name: &str) -> Result<String> {
    let lower = url.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        bail!("{name} must be an http:// or https:// URL, not {url}");
    }
    Ok(url.trim_end_matches('/').to_string())
}

fn list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Parses `kind=prompt|kind=prompt`. An entry without `kind=` is a `query`.
pub fn parse_prompts(value: &str) -> Result<Vec<Prompt>> {
    let mut prompts = Vec::new();
    for entry in value.split('|').map(str::trim).filter(|e| !e.is_empty()) {
        let (kind, text) = match entry.split_once('=') {
            Some((kind, text))
                if !kind.trim().is_empty()
                    && kind
                        .trim()
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') =>
            {
                (kind.trim(), text.trim())
            }
            _ => ("query", entry),
        };
        if text.is_empty() {
            bail!("ANYTOPDF_VLM_PROMPTS entry {entry:?} has no prompt text");
        }
        prompts.push(Prompt {
            kind: kind.into(),
            text: text.into(),
        });
    }
    if prompts.is_empty() {
        bail!("ANYTOPDF_VLM_PROMPTS has no prompts");
    }
    Ok(prompts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Option<Config>> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn unset_url_leaves_the_plugin_inert() {
        assert!(config(&[]).unwrap().is_none());
        assert!(config(&[("ANYTOPDF_VLM_URL", "  ")]).unwrap().is_none());
    }

    #[test]
    fn defaults_match_the_original_script_prompts() {
        let c = config(&[("ANYTOPDF_VLM_URL", "http://127.0.0.1:11434/v1/")])
            .unwrap()
            .unwrap();
        assert_eq!(c.vision.url, "http://127.0.0.1:11434/v1");
        assert_eq!(c.vision.engine, Engine::OpenAi);
        let kinds: Vec<_> = c.prompts.iter().map(|p| p.kind.as_str()).collect();
        assert_eq!(kinds, ["caption", "query", "activity"]);
        assert!(c.detect.is_empty());
        assert_eq!(c.text.unwrap().url, c.vision.url);
        assert_eq!(c.budget, Duration::from_secs(50));
    }

    #[test]
    fn prompts_parse_kinds_and_default_to_query() {
        let prompts =
            parse_prompts("caption=Describe it. | Is there a logo? | brand-1=Name brands").unwrap();
        assert_eq!(prompts[0].kind, "caption");
        assert_eq!(prompts[1].kind, "query");
        assert_eq!(prompts[1].text, "Is there a logo?");
        assert_eq!(prompts[2].kind, "brand-1");
        // `=` inside free text does not make a kind.
        assert_eq!(parse_prompts("Is 2+2=4?").unwrap()[0].kind, "query");
        assert!(parse_prompts("caption=").is_err());
        assert!(parse_prompts(" | ").is_err());
    }

    #[test]
    fn moondream_engine_needs_a_separate_text_endpoint_for_summaries() {
        let base = [
            ("ANYTOPDF_VLM_URL", "http://localhost:2020/v1"),
            ("ANYTOPDF_VLM_ENGINE", "moondream"),
        ];
        assert!(config(&base).unwrap().unwrap().text.is_none());
        let mut with_llm = base.to_vec();
        with_llm.push(("ANYTOPDF_LLM_URL", "http://localhost:11434/v1"));
        with_llm.push(("ANYTOPDF_LLM_MODEL", "llama3.2"));
        let text = config(&with_llm).unwrap().unwrap().text.unwrap();
        assert_eq!(
            (text.engine, text.model.as_str()),
            (Engine::OpenAi, "llama3.2")
        );
        let mut off = with_llm.clone();
        off.push(("ANYTOPDF_VLM_SUMMARY", "off"));
        assert!(config(&off).unwrap().unwrap().text.is_none());
    }

    #[test]
    fn invalid_values_are_errors() {
        let url = ("ANYTOPDF_VLM_URL", "http://localhost:1/v1");
        assert!(config(&[("ANYTOPDF_VLM_URL", "localhost:11434")]).is_err());
        assert!(config(&[url, ("ANYTOPDF_VLM_ENGINE", "gpt")]).is_err());
        assert!(config(&[url, ("ANYTOPDF_VLM_TIMEOUT", "0")]).is_err());
        assert!(config(&[url, ("ANYTOPDF_VLM_MAX_SIDE", "8")]).is_err());
        let c = config(&[url, ("ANYTOPDF_VLM_DETECT", "red car, logo,,")])
            .unwrap()
            .unwrap();
        assert_eq!(c.detect, ["red car", "logo"]);
    }
}
