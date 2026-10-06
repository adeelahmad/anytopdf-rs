//! Chat exports (WhatsApp, Telegram, Slack, iMessage) parsed into one neutral
//! [`Conversation`] model. Each format is a recognizer over its export's own
//! shape; the importer in `importers::chat` renders conversations and pulls
//! attachments in through the registry.

pub(crate) mod imessage;
pub(crate) mod slack;
pub(crate) mod telegram;
pub(crate) mod whatsapp;

/// How numeric dates such as `03/04/2026` are read when the export does not
/// say. `Auto` picks day-first unless a value only fits month-first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatDateOrder {
    #[default]
    Auto,
    Dmy,
    Mdy,
    Ymd,
}

impl std::str::FromStr for ChatDateOrder {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "dmy" => Ok(Self::Dmy),
            "mdy" => Ok(Self::Mdy),
            "ymd" => Ok(Self::Ymd),
            other => Err(format!(
                "unknown chat date order {other:?} (expected auto, dmy, mdy or ymd)"
            )),
        }
    }
}

/// Options for the chat-export importer.
#[derive(Debug, Clone)]
pub struct ChatOptions {
    /// Import attachments that ship inside the export (photos, documents).
    pub attachments: bool,
    /// How ambiguous numeric dates are read (WhatsApp).
    pub date_order: ChatDateOrder,
}

impl Default for ChatOptions {
    fn default() -> Self {
        Self {
            attachments: true,
            date_order: ChatDateOrder::Auto,
        }
    }
}

/// A file a message refers to, as the export names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AttachmentRef {
    /// Path or name exactly as written in the export.
    pub reference: String,
    /// What the export calls it (`photo`, `voice message`); `None` is a plain
    /// attachment.
    pub kind: Option<String>,
}

impl AttachmentRef {
    pub fn new(reference: impl Into<String>) -> Self {
        Self {
            reference: reference.into(),
            kind: None,
        }
    }

    /// The file name alone; full paths from the export never reach the page.
    pub fn display_name(&self) -> &str {
        self.reference
            .rsplit(['/', '\\'])
            .next()
            .filter(|n| !n.is_empty())
            .unwrap_or(&self.reference)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Message {
    /// `YYYY-MM-DD`.
    pub date: Option<String>,
    /// `HH:MM` or `HH:MM:SS`.
    pub time: Option<String>,
    /// `None` for system notices (joins, encryption banners).
    pub sender: Option<String>,
    pub text: String,
    pub attachments: Vec<AttachmentRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conversation {
    /// `WhatsApp`, `Telegram`, `Slack` or `iMessage`.
    pub platform: &'static str,
    pub title: String,
    pub messages: Vec<Message>,
}

impl Conversation {
    /// Distinct senders in order of first appearance.
    pub fn participants(&self) -> Vec<&str> {
        let mut seen = std::collections::HashSet::new();
        self.messages
            .iter()
            .filter_map(|m| m.sender.as_deref())
            .filter(|s| seen.insert(*s))
            .collect()
    }
}

/// `(year, month, day)` for days since 1970-01-01 (proleptic Gregorian).
pub(crate) fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// UTC `(YYYY-MM-DD, HH:MM:SS)` for a Unix timestamp in seconds.
pub(crate) fn utc_date_time(seconds: i64) -> (String, String) {
    let (y, m, d) = civil_from_days(seconds.div_euclid(86_400));
    let secs = seconds.rem_euclid(86_400);
    (
        format!("{y:04}-{m:02}-{d:02}"),
        format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60),
    )
}

/// 24-hour `HH:MM[:SS]` from clock parts, applying an `AM`/`PM` marker.
pub(crate) fn clock(hour: u32, minute: u32, second: Option<u32>, pm: Option<bool>) -> String {
    let hour = match pm {
        Some(true) if hour < 12 => hour + 12,
        Some(false) if hour == 12 => 0,
        _ => hour,
    };
    match second {
        Some(s) => format!("{hour:02}:{minute:02}:{s:02}"),
        None => format!("{hour:02}:{minute:02}"),
    }
}

/// Drops invisible direction marks that exports scatter through lines.
pub(crate) fn strip_marks(line: &str) -> String {
    line.chars()
        .filter(|c| {
            !matches!(
                c,
                '\u{200e}' | '\u{200f}' | '\u{feff}' | '\u{202a}'..='\u{202e}'
            )
        })
        .map(|c| {
            if c == '\u{202f}' || c == '\u{a0}' {
                ' '
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_timestamps_convert_to_utc_calendar_dates() {
        assert_eq!(utc_date_time(0), ("1970-01-01".into(), "00:00:00".into()));
        assert_eq!(
            utc_date_time(1_791_279_850),
            ("2026-10-06".into(), "09:44:10".into())
        );
        assert_eq!(utc_date_time(951_782_400).0, "2000-02-29");
        assert_eq!(utc_date_time(-86_400).0, "1969-12-31");
    }

    #[test]
    fn twelve_hour_clock_converts_to_twenty_four_hour() {
        assert_eq!(clock(12, 5, None, Some(false)), "00:05");
        assert_eq!(clock(12, 5, None, Some(true)), "12:05");
        assert_eq!(clock(9, 44, Some(10), Some(true)), "21:44:10");
        assert_eq!(clock(9, 44, None, None), "09:44");
    }

    #[test]
    fn attachment_display_name_hides_directories() {
        assert_eq!(
            AttachmentRef::new("/Users/a/Library/Messages/IMG_1.HEIC").display_name(),
            "IMG_1.HEIC"
        );
        assert_eq!(AttachmentRef::new("photos\\p.jpg").display_name(), "p.jpg");
        assert_eq!(AttachmentRef::new("x.pdf").display_name(), "x.pdf");
    }

    #[test]
    fn participants_are_distinct_in_first_seen_order() {
        let msg = |s: Option<&str>| Message {
            sender: s.map(str::to_string),
            ..Default::default()
        };
        let conv = Conversation {
            platform: "WhatsApp",
            title: "t".into(),
            messages: vec![msg(Some("Bo")), msg(None), msg(Some("Al")), msg(Some("Bo"))],
        };
        assert_eq!(conv.participants(), ["Bo", "Al"]);
    }
}
