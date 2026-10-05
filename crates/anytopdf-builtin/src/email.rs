//! Email parsing shared by the `.eml`/`.mbox` importer and by callers that
//! receive raw RFC 5322 messages themselves (for example a mail watcher).

use crate::html::html_to_text;
use anyhow::Result;
use mail_parser::{Address, MessageParser, MimeHeaders};
use std::io::BufReader;

/// Attachments beyond this count per message are dropped with a warning.
pub const MAX_ATTACHMENTS: usize = 256;

/// A parsed email message reduced to what a searchable page needs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Email {
    pub subject: Option<String>,
    pub from: Vec<String>,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    /// RFC 3339 date from the `Date` header.
    pub date: Option<String>,
    pub message_id: Option<String>,
    /// Plain-text body; HTML-only bodies are converted to text.
    pub body: String,
    pub attachments: Vec<EmailAttachment>,
    /// Attachments that exceeded [`MAX_ATTACHMENTS`] and were dropped.
    pub dropped_attachments: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailAttachment {
    /// The declared file name, or a generated `attachment-N.<ext>`. Untrusted:
    /// sanitize before using it as a path.
    pub filename: String,
    pub content_type: String,
    pub data: Vec<u8>,
}

/// Parses one RFC 5322 message.
pub fn parse_email(raw: &[u8]) -> Result<Email> {
    let message = MessageParser::default()
        .parse(raw)
        .filter(|m| {
            m.from().is_some()
                || m.to().is_some()
                || m.subject().is_some()
                || m.date().is_some()
                || m.message_id().is_some()
        })
        .ok_or_else(|| anyhow::anyhow!("not an RFC 5322 email message"))?;

    let body = match message.text_part(0) {
        Some(part) if part.is_text_html() => {
            html_to_text(part.text_contents().unwrap_or_default()).text
        }
        Some(part) => part
            .text_contents()
            .map(str::to_string)
            .unwrap_or_else(|| String::from_utf8_lossy(part.contents()).into_owned()),
        None => message
            .html_part(0)
            .and_then(|p| p.text_contents())
            .map(|html| html_to_text(html).text)
            .unwrap_or_default(),
    };

    let mut attachments = Vec::new();
    let mut dropped_attachments = 0;
    for part in message.attachments() {
        // Inline parts with a Content-ID are resources the HTML body embeds
        // (logos, signatures), not documents.
        let inline_resource = part.content_id().is_some()
            && part
                .content_disposition()
                .is_none_or(|d| !d.is_attachment());
        if inline_resource || part.is_empty() {
            continue;
        }
        if attachments.len() == MAX_ATTACHMENTS {
            dropped_attachments += 1;
            continue;
        }
        let content_type = part
            .content_type()
            .map(|ct| match ct.subtype() {
                Some(sub) => format!("{}/{}", ct.ctype(), sub),
                None => ct.ctype().to_string(),
            })
            .unwrap_or_else(|| {
                if part.is_message() {
                    "message/rfc822".into()
                } else {
                    "application/octet-stream".into()
                }
            })
            .to_ascii_lowercase();
        let filename = part
            .attachment_name()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                let n = attachments.len() + 1;
                match extension_for(&content_type, part.is_message()) {
                    Some(ext) => format!("attachment-{n}.{ext}"),
                    None => format!("attachment-{n}"),
                }
            });
        attachments.push(EmailAttachment {
            filename,
            content_type,
            data: part.contents().to_vec(),
        });
    }

    Ok(Email {
        subject: message.subject().map(|s| s.trim().to_string()),
        from: addresses(message.from()),
        to: addresses(message.to()),
        cc: addresses(message.cc()),
        date: message.date().map(|d| d.to_rfc3339()),
        message_id: message.message_id().map(str::to_string),
        body,
        attachments,
        dropped_attachments,
    })
}

fn addresses(address: Option<&Address<'_>>) -> Vec<String> {
    address
        .into_iter()
        .flat_map(|a| a.iter())
        .filter_map(|a| match (a.name(), a.address()) {
            (Some(name), Some(addr)) if !name.trim().is_empty() => {
                Some(format!("{} <{addr}>", name.trim()))
            }
            (_, Some(addr)) => Some(addr.to_string()),
            (Some(name), None) => Some(name.trim().to_string()),
            (None, None) => None,
        })
        .collect()
}

fn extension_for(content_type: &str, is_message: bool) -> Option<&'static str> {
    if is_message {
        return Some("eml");
    }
    Some(match content_type {
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" | "image/jpg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/tiff" => "tiff",
        "image/heic" => "heic",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/markdown" => "md",
        "text/calendar" => "ics",
        "message/rfc822" => "eml",
        "application/zip" => "zip",
        _ => return None,
    })
}

impl Email {
    /// The page text: a header block, a blank line, then the body.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        let mut line = |label: &str, value: &str| {
            if !value.is_empty() {
                out.push_str(label);
                out.push_str(": ");
                out.push_str(value);
                out.push('\n');
            }
        };
        line("Subject", self.subject.as_deref().unwrap_or_default());
        line("From", &self.from.join(", "));
        line("To", &self.to.join(", "));
        line("Cc", &self.cc.join(", "));
        line("Date", self.date.as_deref().unwrap_or_default());
        let names: Vec<&str> = self
            .attachments
            .iter()
            .map(|a| a.filename.as_str())
            .collect();
        line("Attachments", &names.join(", "));
        let body = self.body.trim();
        if !body.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(body);
            out.push('\n');
        }
        out
    }
}

/// Splits an mbox file into raw messages, dropping each `From ` separator line.
pub fn split_mbox(raw: &[u8]) -> Result<Vec<Vec<u8>>> {
    mail_parser::mailbox::mbox::MessageIterator::new(BufReader::new(raw))
        .map(|m| Ok(m?.unwrap_contents()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MULTIPART: &[u8] = b"From: \"Ada Lovelace\" <ada@example.com>\r\n\
To: bob@example.com, Carol <carol@example.com>\r\n\
Subject: =?UTF-8?Q?Caf=C3=A9_invoice?=\r\n\
Date: Mon, 5 Oct 2026 09:30:00 +0000\r\n\
Message-ID: <abc@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b1\"\r\n\
\r\n\
--b1\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
Total due: 42 EUR\r\n\
--b1\r\n\
Content-Type: text/plain; name=\"notes.txt\"\r\n\
Content-Disposition: attachment; filename=\"../../notes.txt\"\r\n\
\r\n\
attached notes\r\n\
--b1\r\n\
Content-Type: image/png\r\n\
Content-ID: <logo>\r\n\
Content-Disposition: inline\r\n\
Content-Transfer-Encoding: base64\r\n\
\r\n\
iVBORw0KGgo=\r\n\
--b1--\r\n";

    #[test]
    fn multipart_message_yields_headers_body_and_real_attachments() {
        let email = parse_email(MULTIPART).unwrap();
        assert_eq!(email.subject.as_deref(), Some("Café invoice"));
        assert_eq!(email.from, ["Ada Lovelace <ada@example.com>"]);
        assert_eq!(email.to, ["bob@example.com", "Carol <carol@example.com>"]);
        assert_eq!(email.date.as_deref(), Some("2026-10-05T09:30:00Z"));
        assert_eq!(email.message_id.as_deref(), Some("abc@example.com"));
        assert_eq!(email.body.trim(), "Total due: 42 EUR");
        assert_eq!(email.attachments.len(), 1, "{:?}", email.attachments);
        assert_eq!(email.attachments[0].filename, "../../notes.txt");
        assert_eq!(email.attachments[0].data, b"attached notes");
        assert_eq!(
            email.to_text(),
            "Subject: Café invoice\nFrom: Ada Lovelace <ada@example.com>\n\
             To: bob@example.com, Carol <carol@example.com>\n\
             Date: 2026-10-05T09:30:00Z\nAttachments: ../../notes.txt\n\nTotal due: 42 EUR\n"
        );
    }

    #[test]
    fn html_only_body_is_converted_to_text() {
        let raw = b"From: a@example.com\r\nSubject: hi\r\nContent-Type: text/html\r\n\r\n\
<html><style>p{}</style><body><p>Hello <b>there</b></p><ul><li>one</li></ul></body></html>\r\n";
        let email = parse_email(raw).unwrap();
        assert_eq!(email.body, "Hello there\n\n- one\n");
    }

    #[test]
    fn unnamed_attachments_get_typed_names_and_nested_messages_are_eml() {
        let raw = b"From: a@example.com\r\nSubject: fwd\r\n\
Content-Type: multipart/mixed; boundary=x\r\n\r\n\
--x\r\nContent-Type: text/plain\r\n\r\nsee attached\r\n\
--x\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment\r\n\r\n%PDF-1.4\r\n\
--x\r\nContent-Type: message/rfc822\r\n\r\nFrom: b@example.com\r\nSubject: inner\r\n\r\ninner body\r\n\
--x--\r\n";
        let email = parse_email(raw).unwrap();
        let names: Vec<&str> = email
            .attachments
            .iter()
            .map(|a| a.filename.as_str())
            .collect();
        assert_eq!(names, ["attachment-1.pdf", "attachment-2.eml"]);
        let inner = parse_email(&email.attachments[1].data).unwrap();
        assert_eq!(inner.subject.as_deref(), Some("inner"));
    }

    #[test]
    fn non_email_input_is_rejected() {
        assert!(parse_email(b"").is_err());
        assert!(parse_email(b"\x00\x01\x02 binary").is_err());
    }

    #[test]
    fn mbox_splits_on_from_lines() {
        let raw = b"From alice@example.com Mon Oct  5 09:00:00 2026\n\
From: alice@example.com\nSubject: one\n\nfirst\n\n\
From bob@example.com Mon Oct  5 10:00:00 2026\n\
From: bob@example.com\nSubject: two\n\nsecond\n";
        let messages = split_mbox(raw).unwrap();
        assert_eq!(messages.len(), 2);
        let subjects: Vec<_> = messages
            .iter()
            .map(|m| parse_email(m).unwrap().subject.unwrap())
            .collect();
        assert_eq!(subjects, ["one", "two"]);
    }
}
