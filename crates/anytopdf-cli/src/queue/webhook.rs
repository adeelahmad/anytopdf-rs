//! Standard Webhooks (https://www.standardwebhooks.com) delivery with a durable outbox.

use super::{Job, Queue, now_secs, rfc3339};
use anyhow::{Context, Result, bail};
use anytopdf_core::atomic_write;
use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const WEBHOOK_SCHEMA: &str = "anytopdf.webhook/1";

/// Seconds to wait after each failed attempt; one more attempt than delays in total.
pub(crate) const RETRY_DELAYS: [u64; 7] = [5, 300, 1800, 7200, 18_000, 36_000, 36_000];

const SECRET_PREFIX: &str = "whsec_";

pub(crate) struct Webhooks {
    urls: Vec<String>,
    key: Vec<u8>,
    agent: ureq::Agent,
}

/// One message to one endpoint, persisted until it is delivered or given up.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Delivery {
    pub(crate) msg_id: String,
    pub(crate) url: String,
    pub(crate) body: String,
    pub(crate) attempts: u32,
    pub(crate) next_attempt_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) last_result: Option<String>,
}

pub(crate) fn decode_secret(secret: &str) -> Result<Vec<u8>> {
    let encoded = secret.trim();
    let encoded = encoded.strip_prefix(SECRET_PREFIX).unwrap_or(encoded);
    let key = STANDARD
        .decode(encoded)
        .context("ANYTOPDF_WEBHOOK_SECRET must be base64, optionally prefixed with whsec_")?;
    if key.len() < 16 {
        bail!("ANYTOPDF_WEBHOOK_SECRET must decode to at least 16 bytes");
    }
    Ok(key)
}

pub(crate) fn new_secret() -> String {
    let mut key = Vec::with_capacity(32);
    key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    format!("{SECRET_PREFIX}{}", STANDARD.encode(key))
}

/// `v1,<base64 HMAC-SHA256 of "id.timestamp.body">`.
pub(crate) fn sign(key: &[u8], msg_id: &str, timestamp: u64, body: &str) -> String {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(format!("{msg_id}.{timestamp}.{body}").as_bytes());
    format!("v1,{}", STANDARD.encode(mac.finalize().into_bytes()))
}

/// Payload `data`: the job's identity and outcome, without absolute paths or error text.
pub(crate) fn job_data(queue: &Queue, job: &Job) -> Value {
    let mut data = json!({
        "job_id": job.id,
        "state": job.state.as_str(),
        "origin": job.origin,
        "inputs": job.inputs.iter().map(|p| anytopdf_core::basename(p)).collect::<Vec<_>>(),
    });
    if let Some(output) = job.output.as_deref().and_then(|p| queue.relative(p)) {
        data["output"] = output.into();
    }
    if let Some(status) = &job.status {
        data["status"] = status.as_str().into();
    }
    if let Some(code) = job.exit_code {
        data["exit_code"] = code.into();
    }
    if let Some(pages) = job.pages {
        data["pages"] = pages.into();
    }
    data
}

pub(crate) fn payload(event: &str, timestamp: u64, data: Value) -> Value {
    json!({
        "schema_version": WEBHOOK_SCHEMA,
        "type": event,
        "timestamp": rfc3339(timestamp),
        "data": data,
    })
}

impl Webhooks {
    pub(crate) fn new(urls: &[String], secret: &str) -> Result<Webhooks> {
        for url in urls {
            let lower = url.to_ascii_lowercase();
            if !(lower.starts_with("https://") || lower.starts_with("http://")) {
                bail!("--webhook must be an http:// or https:// URL: {url}");
            }
        }
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .max_redirects(0)
            .user_agent(concat!("anytopdf/", env!("CARGO_PKG_VERSION")))
            .build();
        Ok(Webhooks {
            urls: urls.to_vec(),
            key: decode_secret(secret)?,
            agent: config.into(),
        })
    }

    pub(crate) fn plain_http(&self) -> impl Iterator<Item = &str> {
        self.urls
            .iter()
            .map(String::as_str)
            .filter(|u| u.to_ascii_lowercase().starts_with("http://"))
    }

    /// Persist one delivery per endpoint; they are sent by [`Webhooks::deliver_due`].
    pub(crate) fn enqueue(&self, queue: &Queue, event: &str, job: &Job) -> Result<()> {
        let now = now_secs();
        let msg_id = format!("msg_{}", uuid::Uuid::now_v7().simple());
        let body = serde_json::to_string(&payload(event, now, job_data(queue, job)))?;
        for (n, url) in self.urls.iter().enumerate() {
            let delivery = Delivery {
                msg_id: msg_id.clone(),
                url: url.clone(),
                body: body.clone(),
                attempts: 0,
                next_attempt_at: now,
                last_result: None,
            };
            let path = queue
                .webhooks_dir("pending")
                .join(format!("{msg_id}-{n}.json"));
            atomic_write(&path, &serde_json::to_vec_pretty(&delivery)?)?;
        }
        Ok(())
    }

    /// Attempt every pending delivery that is due. Returns log lines for the worker.
    pub(crate) fn deliver_due(&self, queue: &Queue) -> Result<Vec<String>> {
        let mut log = Vec::new();
        let mut paths: Vec<PathBuf> = std::fs::read_dir(queue.webhooks_dir("pending"))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        for path in paths {
            let Ok(mut delivery) = read_delivery(&path) else {
                continue;
            };
            if delivery.next_attempt_at > now_secs() {
                continue;
            }
            let result = self.send(&delivery);
            delivery.attempts += 1;
            let outcome = match &result {
                Ok(code) => format!("HTTP {code}"),
                Err(e) => format!("{e:#}"),
            };
            delivery.last_result = Some(outcome.clone());
            let event = event_type(&delivery.body);
            match result {
                Ok(code) if (200..300).contains(&code) => {
                    std::fs::remove_file(&path)?;
                    log.push(format!("webhook {event} {} delivered", delivery.msg_id));
                }
                other => {
                    let gone = matches!(other, Ok(410));
                    match next_delay(delivery.attempts).filter(|_| !gone) {
                        Some(delay) => {
                            delivery.next_attempt_at = now_secs() + delay;
                            atomic_write(&path, &serde_json::to_vec_pretty(&delivery)?)?;
                            log.push(format!(
                                "webhook {event} {} failed ({outcome}); retry in {delay}s",
                                delivery.msg_id
                            ));
                        }
                        None => {
                            let failed = queue
                                .webhooks_dir("failed")
                                .join(path.file_name().unwrap_or_default());
                            atomic_write(&failed, &serde_json::to_vec_pretty(&delivery)?)?;
                            std::fs::remove_file(&path)?;
                            log.push(format!(
                                "webhook {event} {} failed ({outcome}); giving up",
                                delivery.msg_id
                            ));
                        }
                    }
                }
            }
        }
        Ok(log)
    }

    fn send(&self, delivery: &Delivery) -> Result<u16> {
        let timestamp = now_secs();
        let signature = sign(&self.key, &delivery.msg_id, timestamp, &delivery.body);
        let response = self
            .agent
            .post(&delivery.url)
            .header("content-type", "application/json")
            .header("webhook-id", &delivery.msg_id)
            .header("webhook-timestamp", &timestamp.to_string())
            .header("webhook-signature", &signature)
            .send(delivery.body.as_str())?;
        Ok(response.status().as_u16())
    }
}

/// Delay before the next attempt after `attempts` failures, or `None` once exhausted.
pub(crate) fn next_delay(attempts: u32) -> Option<u64> {
    RETRY_DELAYS.get(attempts.checked_sub(1)? as usize).copied()
}

fn event_type(body: &str) -> String {
    serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v["type"].as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn read_delivery(path: &Path) -> Result<Delivery> {
    Ok(serde_json::from_slice(&std::fs::read(path)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_the_standard_webhooks_test_vector() {
        let key = decode_secret("whsec_MfKQ9r8GKYqrTwjUPD8ILPZIo2LaLaSw").unwrap();
        assert_eq!(
            sign(
                &key,
                "msg_p5jXN8AQM9LWM0D4loKWxJek",
                1_614_265_330,
                r#"{"test": 2432232314}"#
            ),
            "v1,g0hM9SsE+OTPJTGt/tmIKtSyZlE3uFJELVlNIOLJ1OE="
        );
    }

    #[test]
    fn secrets_round_trip_and_short_or_invalid_ones_are_rejected() {
        let secret = new_secret();
        assert!(secret.starts_with("whsec_"));
        assert_eq!(decode_secret(&secret).unwrap().len(), 32);
        assert_ne!(secret, new_secret());
        assert!(decode_secret("whsec_not base64!").is_err());
        assert!(decode_secret("whsec_c2hvcnQ=").is_err());
    }

    #[test]
    fn retry_schedule_follows_the_standard_and_then_stops() {
        assert_eq!(next_delay(0), None);
        assert_eq!(next_delay(1), Some(5));
        assert_eq!(next_delay(2), Some(300));
        assert_eq!(next_delay(7), Some(36_000));
        assert_eq!(next_delay(8), None);
    }

    #[test]
    fn webhook_urls_must_be_http() {
        let secret = new_secret();
        assert!(Webhooks::new(&["ftp://example.com/hook".into()], &secret).is_err());
        assert!(Webhooks::new(&["https://example.com/hook".into()], &secret).is_ok());
    }
}
