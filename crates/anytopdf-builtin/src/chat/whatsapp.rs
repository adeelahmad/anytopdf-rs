//! WhatsApp "Export chat" text: `_chat.txt` (iOS, `[date, time] sender: text`)
//! or `WhatsApp Chat with X.txt` (Android, `date, time - sender: text`).

use super::{AttachmentRef, ChatDateOrder, Conversation, Message, clock, strip_marks};
use regex::Regex;
use std::sync::LazyLock;

const STAMP: &str = r"(\d{1,4})[./-](\d{1,2})[./-](\d{2,4}),? (\d{1,2})[:.](\d{2})(?:[:.](\d{2}))?(?: ?([AaPp])\.? ?[Mm]\.?)?";

static IOS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^\[{STAMP}\] (.*)$")).unwrap());
static ANDROID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"^{STAMP} [-–] (.*)$")).unwrap());
static ATTACHED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<attached: ([^>]+)>").unwrap());
const FILE_ATTACHED: &str = " (file attached)";

struct Stamp {
    parts: [u32; 3],
    year_digits: usize,
    first_digits: usize,
    time: String,
}

fn stamp_line(line: &str) -> Option<(Stamp, String)> {
    let caps = IOS.captures(line).or_else(|| ANDROID.captures(line))?;
    let num = |i: usize| caps.get(i).and_then(|m| m.as_str().parse::<u32>().ok());
    let pm = caps.get(7).map(|m| m.as_str().eq_ignore_ascii_case("p"));
    let (hour, minute) = (num(4)?, num(5)?);
    if hour > 23 || minute > 59 {
        return None;
    }
    Some((
        Stamp {
            parts: [num(1)?, num(2)?, num(3)?],
            first_digits: caps[1].len(),
            year_digits: caps[3].len(),
            time: clock(hour, minute, num(6), pm),
        },
        caps[8].to_string(),
    ))
}

/// Whether text opens like a WhatsApp export: its first non-empty line is a
/// timestamped message.
pub(crate) fn looks_like(text: &str) -> bool {
    text.lines()
        .map(strip_marks)
        .find(|l| !l.trim().is_empty())
        .and_then(|l| stamp_line(l.trim_end()))
        .is_some()
}

fn resolve_order(order: ChatDateOrder, stamps: &[&Stamp]) -> ChatDateOrder {
    if order != ChatDateOrder::Auto {
        return order;
    }
    if stamps.iter().any(|s| s.first_digits == 4) {
        return ChatDateOrder::Ymd;
    }
    if stamps.iter().any(|s| s.parts[0] > 12) {
        return ChatDateOrder::Dmy;
    }
    if stamps.iter().any(|s| s.parts[1] > 12) {
        return ChatDateOrder::Mdy;
    }
    ChatDateOrder::Dmy
}

fn iso_date(stamp: &Stamp, order: ChatDateOrder) -> Option<String> {
    let [a, b, c] = stamp.parts;
    let (y, m, d) = match order {
        ChatDateOrder::Ymd => (a, b, c),
        ChatDateOrder::Mdy => (c, a, b),
        ChatDateOrder::Dmy | ChatDateOrder::Auto => (c, b, a),
    };
    let y = if order != ChatDateOrder::Ymd && stamp.year_digits == 2 {
        2000 + y
    } else {
        y
    };
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| format!("{y:04}-{m:02}-{d:02}"))
}

/// Splits `sender: text`. A prefix with no `": "`, or one too long to be a
/// name, is a system notice.
fn split_sender(rest: &str) -> (Option<String>, String) {
    match rest.split_once(": ") {
        Some((name, text)) if !name.is_empty() && name.chars().count() <= 64 => {
            (Some(name.trim().to_string()), text.to_string())
        }
        _ => (None, rest.to_string()),
    }
}

/// Pulls attachment markers out of a message body.
fn take_attachments(text: &str) -> (String, Vec<AttachmentRef>) {
    let mut attachments: Vec<AttachmentRef> = ATTACHED
        .captures_iter(text)
        .map(|c| AttachmentRef::new(c[1].trim()))
        .collect();
    let mut body = ATTACHED.replace_all(text, "").into_owned();
    let mut lines: Vec<&str> = body.lines().collect();
    if let Some(first) = lines.first()
        && let Some(name) = first.strip_suffix(FILE_ATTACHED)
    {
        attachments.push(AttachmentRef::new(name.trim()));
        lines.remove(0);
    }
    body = lines.join("\n");
    (body.trim().to_string(), attachments)
}

pub(crate) fn parse(text: &str, title: &str, order: ChatDateOrder) -> Conversation {
    let mut raw: Vec<(Stamp, String)> = Vec::new();
    for line in text.lines() {
        let line = strip_marks(line);
        let line = line.trim_end();
        match stamp_line(line) {
            Some(entry) => raw.push(entry),
            None => {
                if let Some((_, body)) = raw.last_mut() {
                    body.push('\n');
                    body.push_str(line);
                }
            }
        }
    }
    let stamps: Vec<&Stamp> = raw.iter().map(|(s, _)| s).collect();
    let order = resolve_order(order, &stamps);
    let messages = raw
        .iter()
        .map(|(stamp, rest)| {
            let (sender, body) = split_sender(rest);
            let (text, attachments) = take_attachments(&body);
            Message {
                date: iso_date(stamp, order),
                time: Some(stamp.time.clone()),
                sender,
                text,
                attachments,
            }
        })
        .collect();
    Conversation {
        platform: "WhatsApp",
        title: title.to_string(),
        messages,
    }
}

/// A conversation title from an export's file name: `WhatsApp Chat with
/// Family.txt` and `WhatsApp Chat - Family.zip` both give `Family`.
pub(crate) fn title_from_name(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem).trim();
    for prefix in [
        "WhatsApp Chat with ",
        "WhatsApp Chat - ",
        "WhatsApp Chat mit ",
    ] {
        if let Some(rest) = stem.strip_prefix(prefix) {
            return rest.trim().to_string();
        }
    }
    stem.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const IOS_EXPORT: &str = "\u{feff}[06/10/2026, 09:44:10] Family: \u{200e}Messages and calls are end-to-end encrypted.\n\
[06/10/2026, 09:44:12] Adeel: Hello all\n\
second line\n\
[06/10/2026, 21:05:00] Sara: \u{200e}<attached: 00000012-PHOTO-2026-10-06-21-05-00.jpg>\n\
[13/10/2026, 9:05:00\u{202f}PM] Sara: late\n";

    const ANDROID_EXPORT: &str = "10/13/26, 9:05 PM - Messages and calls are end-to-end encrypted.\n\
10/13/26, 9:06 PM - Adeel: IMG-20261013-WA0001.jpg (file attached)\n\
the caption\n\
10/14/26, 12:01 AM - Sara: <Media omitted>\n";

    #[test]
    fn ios_export_parses_senders_continuations_and_attachments() {
        assert!(looks_like(IOS_EXPORT));
        let conv = parse(IOS_EXPORT, "Family", ChatDateOrder::Auto);
        assert_eq!(conv.messages.len(), 4, "{conv:#?}");
        let hello = &conv.messages[1];
        assert_eq!(hello.date.as_deref(), Some("2026-10-06"));
        assert_eq!(hello.time.as_deref(), Some("09:44:12"));
        assert_eq!(hello.sender.as_deref(), Some("Adeel"));
        assert_eq!(hello.text, "Hello all\nsecond line");
        let photo = &conv.messages[2];
        assert_eq!(photo.text, "");
        assert_eq!(
            photo.attachments,
            [AttachmentRef::new("00000012-PHOTO-2026-10-06-21-05-00.jpg")]
        );
        assert_eq!(conv.messages[3].date.as_deref(), Some("2026-10-13"));
        assert_eq!(conv.messages[3].time.as_deref(), Some("21:05:00"));
        assert_eq!(conv.participants(), ["Family", "Adeel", "Sara"]);
    }

    #[test]
    fn android_export_infers_month_first_dates_and_file_attached_lines() {
        assert!(looks_like(ANDROID_EXPORT));
        let conv = parse(ANDROID_EXPORT, "t", ChatDateOrder::Auto);
        assert_eq!(conv.messages.len(), 3);
        assert_eq!(conv.messages[0].sender, None);
        let img = &conv.messages[1];
        assert_eq!(img.date.as_deref(), Some("2026-10-13"));
        assert_eq!(img.time.as_deref(), Some("21:06"));
        assert_eq!(img.text, "the caption");
        assert_eq!(
            img.attachments,
            [AttachmentRef::new("IMG-20261013-WA0001.jpg")]
        );
        assert_eq!(conv.messages[2].time.as_deref(), Some("00:01"));
    }

    #[test]
    fn explicit_date_order_overrides_detection() {
        let text = "03/04/2026, 10:00 - A: hi\n";
        let auto = parse(text, "t", ChatDateOrder::Auto);
        assert_eq!(auto.messages[0].date.as_deref(), Some("2026-04-03"));
        let mdy = parse(text, "t", ChatDateOrder::Mdy);
        assert_eq!(mdy.messages[0].date.as_deref(), Some("2026-03-04"));
        let ymd = parse("2026-03-04, 10:00 - A: hi\n", "t", ChatDateOrder::Auto);
        assert_eq!(ymd.messages[0].date.as_deref(), Some("2026-03-04"));
    }

    #[test]
    fn ordinary_text_is_not_mistaken_for_an_export() {
        assert!(!looks_like("Meeting notes\n[06/10/2026, 09:44:10] A: hi\n"));
        assert!(!looks_like("2026-10-06 09:44:10 INFO started\n"));
        assert!(!looks_like(""));
    }

    #[test]
    fn titles_come_from_export_file_names() {
        assert_eq!(title_from_name("WhatsApp Chat with Family.txt"), "Family");
        assert_eq!(title_from_name("WhatsApp Chat - Team.zip"), "Team");
        assert_eq!(title_from_name("_chat.txt"), "_chat");
    }
}
