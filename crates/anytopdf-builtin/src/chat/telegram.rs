//! Telegram Desktop "Export chat history" in machine-readable JSON
//! (`result.json`): one chat, or a full account export with `chats.list`.

use super::{AttachmentRef, Conversation, Message};
use anyhow::{Context, Result};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use std::sync::LazyLock;

static MESSAGES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""messages"\s*:\s*\["#).unwrap());
static TELEGRAM_KEY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""(date_unixtime|from_id|text_entities)"\s*:"#).unwrap());

/// Whether a JSON prefix looks like a Telegram export.
pub(crate) fn looks_like(prefix: &str) -> bool {
    let trimmed = prefix.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('{') && MESSAGES.is_match(prefix) && TELEGRAM_KEY.is_match(prefix)
}

#[derive(Deserialize)]
struct Export {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    messages: Option<Vec<RawMessage>>,
    #[serde(default)]
    chats: Option<ChatList>,
    #[serde(default)]
    left_chats: Option<ChatList>,
}

#[derive(Deserialize)]
struct ChatList {
    #[serde(default)]
    list: Vec<Chat>,
}

#[derive(Deserialize)]
struct Chat {
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    messages: Vec<RawMessage>,
}

#[derive(Deserialize)]
struct RawMessage {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    date: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    from_id: Option<String>,
    #[serde(default)]
    actor: Option<String>,
    #[serde(default)]
    action: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    text: Value,
    #[serde(default)]
    photo: Option<String>,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    sticker_emoji: Option<String>,
    #[serde(default)]
    forwarded_from: Option<String>,
    #[serde(default)]
    location_information: Option<Location>,
    #[serde(default)]
    contact_information: Option<Contact>,
    #[serde(default)]
    poll: Option<Poll>,
}

#[derive(Deserialize)]
struct Location {
    latitude: f64,
    longitude: f64,
}

#[derive(Deserialize)]
struct Contact {
    #[serde(default)]
    first_name: String,
    #[serde(default)]
    last_name: String,
    #[serde(default)]
    phone_number: String,
}

#[derive(Deserialize)]
struct Poll {
    #[serde(default)]
    question: String,
}

/// Text is a plain string or an array of strings and `{type, text, href}`
/// entities.
fn flatten_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| match part {
                Value::String(s) => s.clone(),
                Value::Object(entity) => {
                    let text = entity.get("text").and_then(Value::as_str).unwrap_or("");
                    match entity.get("href").and_then(Value::as_str) {
                        Some(href) if href != text => format!("{text} ({href})"),
                        _ => text.to_string(),
                    }
                }
                _ => String::new(),
            })
            .collect(),
        _ => String::new(),
    }
}

/// Telegram writes this in place of files the export settings left out.
fn not_included(reference: &str) -> bool {
    reference.starts_with('(')
}

fn convert(raw: RawMessage) -> Message {
    let (date, time) = match raw.date.as_deref().and_then(|d| d.split_once('T')) {
        Some((d, t)) => (Some(d.to_string()), Some(t.to_string())),
        None => (raw.date.clone(), None),
    };
    let service = raw.kind.as_deref() == Some("service");
    let sender = if service {
        raw.actor.clone()
    } else {
        raw.from.clone().or(raw.from_id.clone())
    };
    let mut lines = Vec::new();
    if let Some(from) = &raw.forwarded_from {
        lines.push(format!("[forwarded from {from}]"));
    }
    if service && let Some(action) = &raw.action {
        let mut line = format!("[{}]", action.replace('_', " "));
        if let Some(title) = &raw.title {
            line.push(' ');
            line.push_str(title);
        }
        lines.push(line);
    }
    let text = flatten_text(&raw.text);
    if !text.is_empty() {
        lines.push(text);
    }
    if let Some(emoji) = &raw.sticker_emoji {
        lines.push(format!("[sticker {emoji}]"));
    }
    if let Some(loc) = &raw.location_information {
        lines.push(format!("[location: {}, {}]", loc.latitude, loc.longitude));
    }
    if let Some(c) = &raw.contact_information {
        let name = format!("{} {}", c.first_name, c.last_name);
        lines.push(format!("[contact: {} {}]", name.trim(), c.phone_number));
    }
    if let Some(poll) = &raw.poll {
        lines.push(format!("[poll: {}]", poll.question));
    }
    let mut attachments = Vec::new();
    for (reference, kind) in [
        (raw.photo, Some("photo".to_string())),
        (raw.file, raw.media_type.map(|m| m.replace('_', " "))),
    ] {
        let Some(reference) = reference else { continue };
        if not_included(&reference) {
            let kind = kind.as_deref().unwrap_or("file");
            lines.push(format!("[{kind} not included in export]"));
        } else {
            attachments.push(AttachmentRef { reference, kind });
        }
    }
    Message {
        date,
        time,
        sender,
        text: lines.join("\n"),
        attachments,
    }
}

fn title(name: Option<String>, kind: Option<&str>) -> String {
    name.filter(|n| !n.is_empty())
        .unwrap_or_else(|| match kind {
            Some("saved_messages") => "Saved Messages".into(),
            Some(kind) => kind.replace('_', " "),
            None => "Telegram chat".into(),
        })
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Conversation>> {
    let export: Export = serde_json::from_slice(bytes).context("parse Telegram export")?;
    let mut conversations = Vec::new();
    if let Some(messages) = export.messages {
        conversations.push(Conversation {
            platform: "Telegram",
            title: title(export.name, export.kind.as_deref()),
            messages: messages.into_iter().map(convert).collect(),
        });
    }
    for chat in export
        .chats
        .into_iter()
        .chain(export.left_chats)
        .flat_map(|list| list.list)
    {
        conversations.push(Conversation {
            platform: "Telegram",
            title: title(chat.name, chat.kind.as_deref()),
            messages: chat.messages.into_iter().map(convert).collect(),
        });
    }
    anyhow::ensure!(
        !conversations.is_empty(),
        "Telegram export has no messages or chats"
    );
    Ok(conversations)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SINGLE: &str = r#"{
 "name": "Family",
 "type": "private_group",
 "id": 42,
 "messages": [
  {"id": 1, "type": "service", "date": "2026-10-06T09:40:00", "date_unixtime": "1791279600",
   "actor": "Adeel", "actor_id": "user1", "action": "create_group", "title": "Family", "text": ""},
  {"id": 2, "type": "message", "date": "2026-10-06T09:44:10", "date_unixtime": "1791279850",
   "from": "Adeel", "from_id": "user1",
   "text": ["See ", {"type": "text_link", "text": "this", "href": "https://example.com"}, " now"],
   "text_entities": []},
  {"id": 3, "type": "message", "date": "2026-10-06T09:45:00", "from": null, "from_id": "user2",
   "photo": "photos/photo_1@06-10-2026_09-45-00.jpg", "text": "beach"},
  {"id": 4, "type": "message", "date": "2026-10-06T09:46:00", "from": "Sara", "from_id": "user2",
   "file": "(File not included. Change data exporting settings to download.)",
   "media_type": "voice_message", "text": ""},
  {"id": 5, "type": "message", "date": "2026-10-06T09:47:00", "from": "Sara", "from_id": "user2",
   "location_information": {"latitude": 51.5, "longitude": -0.12}, "text": ""}
 ]
}"#;

    #[test]
    fn single_chat_export_converts_text_entities_service_and_media() {
        assert!(looks_like(SINGLE));
        let convs = parse(SINGLE.as_bytes()).unwrap();
        assert_eq!(convs.len(), 1);
        let conv = &convs[0];
        assert_eq!(conv.title, "Family");
        let m = &conv.messages;
        assert_eq!(m[0].sender.as_deref(), Some("Adeel"));
        assert_eq!(m[0].text, "[create group] Family");
        assert_eq!(m[1].date.as_deref(), Some("2026-10-06"));
        assert_eq!(m[1].time.as_deref(), Some("09:44:10"));
        assert_eq!(m[1].text, "See this (https://example.com) now");
        assert_eq!(m[2].sender.as_deref(), Some("user2"));
        assert_eq!(
            m[2].attachments,
            [AttachmentRef {
                reference: "photos/photo_1@06-10-2026_09-45-00.jpg".into(),
                kind: Some("photo".into()),
            }]
        );
        assert!(m[3].attachments.is_empty());
        assert_eq!(m[3].text, "[voice message not included in export]");
        assert_eq!(m[4].text, "[location: 51.5, -0.12]");
    }

    #[test]
    fn full_account_export_yields_one_conversation_per_chat() {
        let full = r#"{"about": "x", "chats": {"about": "y", "list": [
            {"name": "A", "type": "personal_chat", "id": 1, "messages": [
              {"id": 1, "type": "message", "date": "2026-01-01T00:00:00", "from": "A", "from_id": "user1", "text": "hi"}]},
            {"type": "saved_messages", "id": 2, "messages": []}
        ]}, "left_chats": {"list": [{"name": "Old", "type": "public_channel", "id": 3, "messages": []}]}}"#;
        let convs = parse(full.as_bytes()).unwrap();
        let titles: Vec<&str> = convs.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["A", "Saved Messages", "Old"]);
        assert_eq!(convs[0].messages[0].text, "hi");
    }

    #[test]
    fn other_json_is_not_claimed() {
        assert!(!looks_like(
            r#"{"messages": [{"role": "user", "content": "hi"}]}"#
        ));
        assert!(!looks_like(r#"[{"date_unixtime": "1", "messages": []}]"#));
        assert!(parse(br#"{"name": "x"}"#).is_err());
    }
}
