//! Slack workspace exports: `users.json`, `channels.json` and one folder per
//! conversation holding a `YYYY-MM-DD.json` array of messages per day. Files
//! shared in Slack are only linked from an export, never included.

use super::{AttachmentRef, Conversation, Message, utc_date_time};
use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::LazyLock;

static DAY_FILE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}\.json$").unwrap());
static MESSAGE_TYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""type"\s*:\s*"message""#).unwrap());
static TS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""ts"\s*:\s*"\d+\.\d+""#).unwrap());
static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([^<>|]+)(?:\|([^<>]*))?>").unwrap());

/// Whether `name` is a Slack day file name.
pub(crate) fn is_day_file_name(name: &str) -> bool {
    DAY_FILE.is_match(name)
}

/// Whether a JSON prefix looks like a Slack day file: an array of messages
/// with Slack `ts` timestamps.
pub(crate) fn looks_like_day(prefix: &str) -> bool {
    let trimmed = prefix.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('[') && MESSAGE_TYPE.is_match(prefix) && TS.is_match(prefix)
}

#[derive(Deserialize)]
struct Profile {
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    real_name: Option<String>,
}

#[derive(Deserialize)]
struct User {
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    real_name: Option<String>,
    #[serde(default)]
    profile: Option<Profile>,
}

fn non_empty(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|s| !s.trim().is_empty())
}

/// User ids mapped to the name people see in Slack.
#[derive(Debug, Default)]
pub(crate) struct Directory {
    users: HashMap<String, String>,
    conversations: HashMap<String, String>,
}

impl Directory {
    /// Reads `users.json`.
    pub fn add_users(&mut self, bytes: &[u8]) -> Result<()> {
        let users: Vec<User> = serde_json::from_slice(bytes).context("parse Slack users.json")?;
        for user in users {
            let profile = user.profile.as_ref();
            let name = profile
                .and_then(|p| non_empty(&p.display_name).or(non_empty(&p.real_name)))
                .or(non_empty(&user.real_name))
                .or(non_empty(&user.name))
                .unwrap_or(&user.id)
                .to_string();
            self.users.insert(user.id, name);
        }
        Ok(())
    }

    /// Reads `channels.json`, `groups.json`, `mpims.json` or `dms.json` so
    /// folders named by id get readable titles.
    pub fn add_conversations(&mut self, bytes: &[u8], prefix: &str) -> Result<()> {
        #[derive(Deserialize)]
        struct Conv {
            #[serde(default)]
            id: Option<String>,
            #[serde(default)]
            name: Option<String>,
            #[serde(default)]
            members: Vec<String>,
        }
        let convs: Vec<Conv> =
            serde_json::from_slice(bytes).context("parse Slack conversation list")?;
        for conv in convs {
            let title = match non_empty(&conv.name) {
                Some(name) => format!("{prefix}{name}"),
                None => {
                    let names: Vec<&str> = conv.members.iter().map(|m| self.user(m)).collect();
                    format!("DM: {}", names.join(", "))
                }
            };
            if let Some(name) = non_empty(&conv.name) {
                self.conversations.insert(name.to_string(), title.clone());
            }
            if let Some(id) = conv.id {
                self.conversations.insert(id, title);
            }
        }
        Ok(())
    }

    fn user<'a>(&'a self, id: &'a str) -> &'a str {
        self.users.get(id).map_or(id, String::as_str)
    }

    /// The title for the export folder `folder`.
    pub fn title(&self, folder: &str) -> String {
        self.conversations
            .get(folder)
            .cloned()
            .unwrap_or_else(|| format!("#{folder}"))
    }
}

#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    ts: Option<String>,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    user_profile: Option<Profile>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    files: Vec<RawFile>,
}

/// Slack markup to plain text: `<@U1>` mentions, `<#C1|general>` channels,
/// `<url|label>` links and HTML entities.
fn plain_text(text: &str, dir: &Directory) -> String {
    let replaced = LINK.replace_all(text, |caps: &regex::Captures<'_>| {
        let target = &caps[1];
        let label = caps.get(2).map(|m| m.as_str()).filter(|l| !l.is_empty());
        if let Some(id) = target.strip_prefix('@') {
            format!("@{}", label.unwrap_or_else(|| dir.user(id)))
        } else if let Some(id) = target.strip_prefix('#') {
            format!("#{}", label.unwrap_or(id))
        } else if let Some(special) = target.strip_prefix('!') {
            format!("@{}", label.unwrap_or(special))
        } else {
            match label {
                Some(label) if label != target => format!("{label} ({target})"),
                _ => target.to_string(),
            }
        }
    });
    replaced
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Parses one day file's messages.
pub(crate) fn parse_day(bytes: &[u8], dir: &Directory) -> Result<Vec<Message>> {
    let raw: Vec<RawMessage> = serde_json::from_slice(bytes).context("parse Slack day file")?;
    Ok(raw
        .into_iter()
        .filter(|m| m.kind.as_deref().is_none_or(|k| k == "message"))
        .map(|m| {
            let seconds =
                m.ts.as_deref()
                    .and_then(|ts| ts.split('.').next())
                    .and_then(|s| s.parse::<i64>().ok());
            let (date, time) = match seconds {
                Some(s) => {
                    let (d, t) = utc_date_time(s);
                    (Some(d), Some(t))
                }
                None => (None, None),
            };
            let sender = m
                .user_profile
                .as_ref()
                .and_then(|p| non_empty(&p.display_name).or(non_empty(&p.real_name)))
                .map(str::to_string)
                .or_else(|| m.user.as_deref().map(|u| dir.user(u).to_string()))
                .or(m.username.clone());
            let mut text = plain_text(&m.text, dir);
            if m.thread_ts.is_some() && m.thread_ts != m.ts {
                text = format!("(thread reply) {text}").trim_end().to_string();
            }
            let attachments = m
                .files
                .iter()
                .filter_map(|f| f.name.clone().or(f.title.clone()))
                .map(|name| AttachmentRef {
                    reference: name,
                    kind: Some("file".into()),
                })
                .collect();
            Message {
                date,
                time,
                sender,
                text,
                attachments,
            }
        })
        .collect())
}

/// Builds one conversation from day files already sorted by date.
pub(crate) fn conversation(title: String, days: Vec<Vec<Message>>) -> Conversation {
    Conversation {
        platform: "Slack",
        title,
        messages: days.into_iter().flatten().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USERS: &str = r#"[
      {"id": "U1", "name": "adeel", "real_name": "Adeel Ahmad", "profile": {"display_name": "", "real_name": "Adeel Ahmad"}},
      {"id": "U2", "name": "sara", "profile": {"display_name": "Sara K"}}
    ]"#;

    const DAY: &str = r#"[
      {"type": "message", "user": "U1", "text": "Hi <@U2>, see <https://example.com|the doc> &amp; <#C9|general>", "ts": "1791279850.000100"},
      {"type": "message", "subtype": "file_share", "user": "U2", "text": "", "ts": "1791279900.000200",
       "thread_ts": "1791279850.000100", "files": [{"name": "plan.pdf", "title": "Plan"}]},
      {"type": "message", "subtype": "bot_message", "username": "ci-bot", "text": "build ok", "ts": "1791279950.000300"}
    ]"#;

    #[test]
    fn day_file_resolves_mentions_links_threads_and_files() {
        assert!(looks_like_day(DAY));
        assert!(is_day_file_name("2026-10-06.json"));
        let mut dir = Directory::default();
        dir.add_users(USERS.as_bytes()).unwrap();
        let msgs = parse_day(DAY.as_bytes(), &dir).unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[0].sender.as_deref(), Some("Adeel Ahmad"));
        assert_eq!(msgs[0].date.as_deref(), Some("2026-10-06"));
        assert_eq!(msgs[0].time.as_deref(), Some("09:44:10"));
        assert_eq!(
            msgs[0].text,
            "Hi @Sara K, see the doc (https://example.com) & #general"
        );
        assert_eq!(msgs[1].sender.as_deref(), Some("Sara K"));
        assert_eq!(msgs[1].text, "(thread reply)");
        assert_eq!(msgs[1].attachments[0].reference, "plan.pdf");
        assert_eq!(msgs[2].sender.as_deref(), Some("ci-bot"));
    }

    #[test]
    fn conversation_titles_come_from_channel_and_dm_lists() {
        let mut dir = Directory::default();
        dir.add_users(USERS.as_bytes()).unwrap();
        dir.add_conversations(br#"[{"id": "C1", "name": "general"}]"#, "#")
            .unwrap();
        dir.add_conversations(br#"[{"id": "D1", "members": ["U1", "U2"]}]"#, "")
            .unwrap();
        assert_eq!(dir.title("general"), "#general");
        assert_eq!(dir.title("D1"), "DM: Adeel Ahmad, Sara K");
        assert_eq!(dir.title("unknown"), "#unknown");
    }

    #[test]
    fn other_json_arrays_are_not_day_files() {
        assert!(!looks_like_day(r#"[{"type": "message", "text": "x"}]"#));
        assert!(!looks_like_day(r#"{"type": "message", "ts": "1.2"}"#));
    }
}
