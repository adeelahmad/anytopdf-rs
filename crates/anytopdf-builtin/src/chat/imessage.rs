//! iMessage conversations exported as text by `imessage-exporter -f txt`:
//! blocks of a `Mon DD, YYYY  H:MM:SS AM` line, a sender line (`Me` for the
//! account owner) and the message body, separated by blank lines.

use super::{AttachmentRef, Conversation, Message, clock, strip_marks};
use regex::Regex;
use std::sync::LazyLock;

static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec) (\d{1,2}), (\d{4}) +(\d{1,2}):(\d{2}):(\d{2}) ([AP]M)\b")
        .unwrap()
});
/// A body line that is only a file path, as the exporter writes attachments.
static PATH_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\S*[/\\][^\s/\\]*\S\.[A-Za-z0-9]{2,5}$").unwrap());

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn header(line: &str) -> Option<(String, String)> {
    let caps = HEADER.captures(line)?;
    let month = MONTHS.iter().position(|m| *m == &caps[1])? + 1;
    let num = |i: usize| caps[i].parse::<u32>().ok();
    let (hour, minute, second) = (num(4)?, num(5)?, num(6)?);
    if hour > 12 || minute > 59 || second > 60 {
        return None;
    }
    Some((
        format!("{}-{month:02}-{:02}", &caps[3], num(2)?),
        clock(hour, minute, Some(second), Some(&caps[7] == "PM")),
    ))
}

/// Whether text opens like an imessage-exporter conversation.
pub(crate) fn looks_like(text: &str) -> bool {
    let mut lines = text
        .lines()
        .map(strip_marks)
        .skip_while(|l| l.trim().is_empty());
    let first = lines.next().unwrap_or_default();
    let sender = lines.next().unwrap_or_default();
    header(first.trim_end()).is_some() && !sender.trim().is_empty()
}

fn finish(messages: &mut Vec<Message>, mut current: Message, body: &mut Vec<String>) {
    while body.last().is_some_and(|l| l.trim().is_empty()) {
        body.pop();
    }
    let mut text = Vec::new();
    for line in body.drain(..) {
        if PATH_LINE.is_match(&line) && !line.contains("://") {
            current.attachments.push(AttachmentRef::new(line));
        } else {
            text.push(line);
        }
    }
    current.text = text.join("\n");
    messages.push(current);
}

pub(crate) fn parse(text: &str, title: &str) -> Conversation {
    let mut messages = Vec::new();
    let mut current: Option<Message> = None;
    let mut body: Vec<String> = Vec::new();
    let mut previous_blank = true;
    let mut lines = text.lines().map(strip_marks).peekable();
    while let Some(line) = lines.next() {
        let line = line.trim_end().to_string();
        if previous_blank && let Some((date, time)) = header(&line) {
            if let Some(done) = current.take() {
                finish(&mut messages, done, &mut body);
            }
            let sender = lines.next().map(|s| s.trim().to_string());
            current = Some(Message {
                date: Some(date),
                time: Some(time),
                sender: sender.filter(|s| !s.is_empty()),
                ..Default::default()
            });
            previous_blank = false;
            continue;
        }
        previous_blank = line.trim().is_empty();
        if current.is_some() {
            body.push(line);
        }
    }
    if let Some(done) = current {
        finish(&mut messages, done, &mut body);
    }
    Conversation {
        platform: "iMessage",
        title: title.to_string(),
        messages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = "May 17, 2026  5:29:42 PM\n\
+15558675309\n\
Hello there\n\
\n\
May 17, 2026  5:30:12 PM (Read by them after 1 minute)\n\
Me\n\
Two lines\n\
of text\n\
https://example.com/page.html\n\
attachments/12/IMG_0001.jpeg\n\
\n\
Jan 02, 2027 12:01:00 AM\n\
+15558675309\n\
Happy new year\n";

    #[test]
    fn exporter_text_parses_headers_senders_bodies_and_attachment_paths() {
        assert!(looks_like(EXPORT));
        let conv = parse(EXPORT, "+15558675309");
        assert_eq!(conv.messages.len(), 3, "{conv:#?}");
        let first = &conv.messages[0];
        assert_eq!(first.date.as_deref(), Some("2026-05-17"));
        assert_eq!(first.time.as_deref(), Some("17:29:42"));
        assert_eq!(first.sender.as_deref(), Some("+15558675309"));
        assert_eq!(first.text, "Hello there");
        let mine = &conv.messages[1];
        assert_eq!(mine.sender.as_deref(), Some("Me"));
        assert_eq!(
            mine.text,
            "Two lines\nof text\nhttps://example.com/page.html"
        );
        assert_eq!(
            mine.attachments,
            [AttachmentRef::new("attachments/12/IMG_0001.jpeg")]
        );
        assert_eq!(conv.messages[2].date.as_deref(), Some("2027-01-02"));
        assert_eq!(conv.messages[2].time.as_deref(), Some("00:01:00"));
    }

    #[test]
    fn prose_mentioning_a_date_is_not_an_export() {
        assert!(!looks_like("Notes\nMay 17, 2026  5:29:42 PM\nMe\n"));
        assert!(!looks_like("May 17, 2026  5:29:42 PM\n"));
        assert!(!looks_like("May 17, 2026 was sunny\nMe\n"));
    }
}
