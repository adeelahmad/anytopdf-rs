//! HTTP clients for OpenAI-compatible chat endpoints and the Moondream API.

use crate::config::{Endpoint, Engine};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// Responses larger than this are not model answers.
const MAX_RESPONSE_BYTES: u64 = 4 * 1024 * 1024;
const MAX_TOKENS: u64 = 300;

/// A normalized box from open-vocabulary detection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    pub x_min: f64,
    pub y_min: f64,
    pub x_max: f64,
    pub y_max: f64,
}

/// A request failure. `unreachable` means later requests in the same
/// invocation would fail the same way, so callers stop early.
#[derive(Debug)]
pub struct CallError {
    pub unreachable: bool,
    pub error: anyhow::Error,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:#}", self.error)
    }
}

impl From<anyhow::Error> for CallError {
    fn from(error: anyhow::Error) -> Self {
        CallError {
            unreachable: false,
            error,
        }
    }
}

pub struct Client {
    endpoint: Endpoint,
    deadline: Instant,
}

impl Client {
    pub fn new(endpoint: Endpoint, deadline: Instant) -> Client {
        Client { endpoint, deadline }
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// Asks one prompt about a JPEG image. `kind == "caption"` uses the
    /// Moondream caption endpoint; everything else is a question.
    pub fn describe(
        &self,
        jpeg_base64: &str,
        kind: &str,
        prompt: &str,
    ) -> Result<String, CallError> {
        let image_url = format!("data:image/jpeg;base64,{jpeg_base64}");
        let answer = match self.endpoint.engine {
            Engine::OpenAi => {
                let body = json!({
                    "model": self.endpoint.model,
                    "messages": [{"role": "user", "content": [
                        {"type": "text", "text": prompt},
                        {"type": "image_url", "image_url": {"url": image_url}}
                    ]}],
                    "max_tokens": MAX_TOKENS,
                    "temperature": 0,
                    "stream": false
                });
                chat_content(&self.post("chat/completions", &body)?)?
            }
            Engine::Moondream if kind == "caption" => {
                let body = json!({"image_url": image_url, "length": "normal", "stream": false});
                string_field(&self.post("caption", &body)?, "caption")?
            }
            Engine::Moondream => {
                let body = json!({"image_url": image_url, "question": prompt, "stream": false});
                string_field(&self.post("query", &body)?, "answer")?
            }
        };
        Ok(clean(&answer))
    }

    /// Open-vocabulary detection; only the Moondream API offers it.
    pub fn detect(&self, jpeg_base64: &str, object: &str) -> Result<Vec<Detection>, CallError> {
        if self.endpoint.engine != Engine::Moondream {
            return Err(CallError {
                unreachable: true,
                error: anyhow!("ANYTOPDF_VLM_DETECT needs ANYTOPDF_VLM_ENGINE=moondream"),
            });
        }
        let body = json!({
            "image_url": format!("data:image/jpeg;base64,{jpeg_base64}"),
            "object": object
        });
        let response = self.post("detect", &body)?;
        let objects = response["objects"]
            .as_array()
            .context("detect response has no objects array")?;
        Ok(objects
            .iter()
            .filter_map(|o| {
                let get = |k: &str| o[k].as_f64().filter(|v| v.is_finite());
                Some(Detection {
                    x_min: get("x_min")?,
                    y_min: get("y_min")?,
                    x_max: get("x_max")?,
                    y_max: get("y_max")?,
                })
            })
            .collect())
    }

    /// A text-only chat completion (summaries, categories and topics). The
    /// answer keeps its line breaks; see [`clean`].
    pub fn complete(&self, prompt: &str) -> Result<String, CallError> {
        if self.endpoint.engine != Engine::OpenAi {
            return Err(CallError {
                unreachable: true,
                error: anyhow!("text completion needs an OpenAI-compatible endpoint"),
            });
        }
        let body = json!({
            "model": self.endpoint.model,
            "messages": [{"role": "user", "content": prompt}],
            "max_tokens": MAX_TOKENS,
            "temperature": 0,
            "stream": false
        });
        Ok(chat_content(&self.post("chat/completions", &body)?)?
            .trim()
            .to_string())
    }

    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, CallError> {
        let remaining = self.remaining();
        if remaining < Duration::from_millis(250) {
            return Err(CallError {
                unreachable: true,
                error: anyhow!(
                    "time budget exhausted (raise ANYTOPDF_VLM_TIMEOUT and --plugin-timeout)"
                ),
            });
        }
        let url = format!("{}/{path}", self.endpoint.url);
        let mut config = ureq::Agent::config_builder()
            .timeout_global(Some(remaining))
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(concat!("anytopdf-plugin-vlm/", env!("CARGO_PKG_VERSION")));
        // A local model server must not be reached through an outbound proxy.
        if is_loopback(&self.endpoint.url) {
            config = config.proxy(None);
        }
        let agent: ureq::Agent = config.build().into();
        let mut request = agent.post(&url).header("content-type", "application/json");
        if let Some(key) = &self.endpoint.api_key {
            request = match self.endpoint.engine {
                Engine::OpenAi => request.header("authorization", &format!("Bearer {key}")),
                Engine::Moondream => request.header("x-moondream-auth", key),
            };
        }
        let payload = serde_json::to_vec(body).map_err(anyhow::Error::from)?;
        let mut response = request.send(&payload[..]).map_err(|e| transport(&url, e))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_string()
            .map_err(|e| transport(&url, e))?;
        if !(200..300).contains(&status) {
            let detail: String = text.trim().chars().take(200).collect();
            return Err(CallError {
                // A missing route or model will not appear for the next frame.
                unreachable: matches!(status, 401 | 403 | 404),
                error: anyhow!("{url} returned HTTP {status}: {detail}"),
            });
        }
        serde_json::from_str(&text)
            .with_context(|| format!("{url} returned invalid JSON"))
            .map_err(CallError::from)
    }
}

fn transport(url: &str, e: ureq::Error) -> CallError {
    let unreachable = match &e {
        ureq::Error::ConnectionFailed | ureq::Error::HostNotFound | ureq::Error::Timeout(_) => true,
        ureq::Error::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::TimedOut
        ),
        _ => false,
    };
    CallError {
        unreachable,
        error: anyhow!("{url}: {e}"),
    }
}

fn is_loopback(url: &str) -> bool {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = rest.split('/').next().unwrap_or_default();
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = authority.strip_prefix('[') {
        v6.split(']').next().unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Reads `choices[0].message.content` as a string or a list of text parts.
fn chat_content(response: &Value) -> Result<String> {
    let content = &response["choices"][0]["message"]["content"];
    if let Some(text) = content.as_str() {
        return Ok(text.to_string());
    }
    if let Some(parts) = content.as_array() {
        return Ok(parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(""));
    }
    bail!("chat response has no choices[0].message.content")
}

fn string_field(response: &Value, field: &str) -> Result<String> {
    response[field]
        .as_str()
        .map(String::from)
        .with_context(|| format!("response has no {field} string"))
}

/// Collapses whitespace so an answer is one searchable line.
pub fn clean(answer: &str) -> String {
    answer.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts_skip_the_proxy() {
        assert!(is_loopback("http://localhost:11434/v1"));
        assert!(is_loopback("http://127.0.0.1:2020/v1"));
        assert!(is_loopback("http://[::1]:8080"));
        assert!(is_loopback("http://user:pw@LOCALHOST/v1"));
        assert!(!is_loopback("https://api.moondream.ai/v1"));
        assert!(!is_loopback("http://192.168.1.4:11434/v1"));
    }

    #[test]
    fn chat_content_accepts_strings_and_text_parts() {
        let plain = json!({"choices": [{"message": {"content": "a dog"}}]});
        assert_eq!(chat_content(&plain).unwrap(), "a dog");
        let parts = json!({"choices": [{"message": {"content": [
            {"type": "text", "text": "a "}, {"type": "text", "text": "cat"}
        ]}}]});
        assert_eq!(chat_content(&parts).unwrap(), "a cat");
        assert!(chat_content(&json!({"error": "x"})).is_err());
    }

    #[test]
    fn answers_collapse_to_one_line() {
        assert_eq!(clean("  A man\n\n walks  a dog. "), "A man walks a dog.");
    }
}
