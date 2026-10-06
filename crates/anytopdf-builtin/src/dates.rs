//! Rule-based date and time mentions ("3 March 2024", "2024-03-03 14:05",
//! "Tuesday at 5pm", "last Friday"). Relative mentions resolve against a
//! reference date, normally the source's capture date, and stay unresolved
//! without one.

use chrono::{Datelike, Days, NaiveDate, NaiveTime, Weekday};
use regex::{Captures, Regex};
use std::sync::LazyLock;

/// How to read an all-numeric date such as `03/04/2024` when either part could
/// be the month.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum DateOrder {
    /// 03/04/2024 is 3 April.
    #[default]
    DayFirst,
    /// 03/04/2024 is 4 March.
    MonthFirst,
}

impl std::str::FromStr for DateOrder {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "dmy" => Ok(Self::DayFirst),
            "mdy" => Ok(Self::MonthFirst),
            other => Err(format!(
                "unknown date order `{other}` (expected dmy or mdy)"
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DateMention {
    pub start: usize,
    pub end: usize,
    /// `date`, `time` or `datetime`.
    pub kind: &'static str,
    /// ISO 8601: `2024-03-03`, `14:05:00`, `2024-03-03T14:05:00`, or `--03-03`
    /// for a day and month with no known year. `None` when a relative mention
    /// has no reference date.
    pub iso: Option<String>,
    /// Resolved (or left unresolved) relative to the reference date.
    pub relative: bool,
}

#[derive(Clone)]
struct DatePart {
    iso: Option<String>,
    relative: bool,
}

struct Piece {
    start: usize,
    end: usize,
    date: Option<DatePart>,
    time: Option<NaiveTime>,
}

const MONTH: &str = r"(jan(?:uary)?|feb(?:ruary)?|mar(?:ch)?|apr(?:il)?|may|june?|july?|aug(?:ust)?|sept?(?:ember)?|oct(?:ober)?|nov(?:ember)?|dec(?:ember)?)\.?";
const WEEKDAY_PREFIX: &str =
    r"(?:(?:mon|tues?|wed(?:nes)?|thu(?:rs?)?|fri|sat(?:ur)?|sun)(?:day)?\.?,?\s+)?";
const WEEKDAY: &str = r"(monday|tuesday|wednesday|thursday|friday|saturday|sunday)";

fn re(pattern: &str) -> Regex {
    Regex::new(&format!("(?i){pattern}")).expect("valid date pattern")
}

static ISO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"\b(\d{4})[-/](\d{1,2})[-/](\d{1,2})(?:(?:T|\s+)(\d{1,2}):(\d{2})(?::(\d{2}))?)?\b")
});
static DAY_MONTH: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"\b{WEEKDAY_PREFIX}(\d{{1,2}})(?:st|nd|rd|th)?(?:\s+of)?\s+{MONTH}(?:,?\s+(\d{{4}}))?\b"
    ))
});
static MONTH_DAY: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"\b{WEEKDAY_PREFIX}{MONTH}\s+(\d{{1,2}})(?:st|nd|rd|th)?(?:,?\s+(\d{{4}}))?\b"
    ))
});
static NUMERIC: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(\d{1,2})([/.])(\d{1,2})[/.](\d{4}|\d{2})\b"));
static DAY_WORD: LazyLock<Regex> = LazyLock::new(|| re(r"\b(today|tonight|yesterday|tomorrow)\b"));
static NAMED_WEEKDAY: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"\b(?:(last|next|this)\s+)?{WEEKDAY}\b")));
static OFFSET: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\b(?:(\d{1,3}|a|one|two|three|four|five|six|seven)\s+(day|week)s?\s+ago|in\s+(\d{1,3}|a|one|two|three|four|five|six|seven)\s+(day|week)s?)\b",
    )
});
static TIME_12H: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(1[0-2]|0?[1-9])(?::([0-5]\d))?(?::([0-5]\d))?\s*([ap])\.?\s?m\b\.?"));
static TIME_24H: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b([01]?\d|2[0-3]):([0-5]\d)(?::([0-5]\d))?\b"));
static DATE_THEN_TIME: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(?:,\s*)?(?:at\s+|@\s*)?$"));
static TIME_THEN_DATE: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(?:,\s*)?(?:on\s+)?$"));

fn month_number(name: &str) -> Option<u32> {
    let prefix = name.get(..3)?.to_ascii_lowercase();
    let months = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    months
        .iter()
        .position(|m| *m == prefix)
        .map(|i| i as u32 + 1)
}

fn weekday(name: &str) -> Option<Weekday> {
    name.get(..3)?.to_ascii_lowercase().parse().ok()
}

fn count(word: &str) -> Option<u64> {
    match word.to_ascii_lowercase().as_str() {
        "a" | "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        digits => digits.parse().ok(),
    }
}

fn number(caps: &Captures, i: usize) -> Option<u32> {
    caps.get(i)?.as_str().parse().ok()
}

fn absolute(date: NaiveDate) -> DatePart {
    DatePart {
        iso: Some(date.format("%Y-%m-%d").to_string()),
        relative: false,
    }
}

fn relative(date: Option<NaiveDate>) -> DatePart {
    DatePart {
        iso: date.map(|d| d.format("%Y-%m-%d").to_string()),
        relative: true,
    }
}

/// A day and month: a full date when the year is written or known from the
/// reference, else ISO 8601's year-less `--MM-DD`.
fn day_month(
    day: u32,
    month: u32,
    year: Option<i32>,
    reference: Option<NaiveDate>,
) -> Option<DatePart> {
    if let Some(year) = year {
        return NaiveDate::from_ymd_opt(year, month, day).map(absolute);
    }
    // 2000 is a leap year, so 29 February passes.
    NaiveDate::from_ymd_opt(2000, month, day)?;
    Some(match reference {
        Some(r) => DatePart {
            relative: true,
            ..absolute(NaiveDate::from_ymd_opt(r.year(), month, day)?)
        },
        None => DatePart {
            iso: Some(format!("--{month:02}-{day:02}")),
            relative: true,
        },
    })
}

fn shift(reference: Option<NaiveDate>, days: i64) -> Option<NaiveDate> {
    let r = reference?;
    if days >= 0 {
        r.checked_add_days(Days::new(days as u64))
    } else {
        r.checked_sub_days(Days::new(days.unsigned_abs()))
    }
}

fn weekday_date(
    reference: Option<NaiveDate>,
    qualifier: Option<&str>,
    day: Weekday,
) -> Option<NaiveDate> {
    let r = reference?;
    let ahead = (day.num_days_from_monday() as i64 - r.weekday().num_days_from_monday() as i64)
        .rem_euclid(7);
    let delta = match qualifier.map(str::to_ascii_lowercase).as_deref() {
        Some("last") => {
            if ahead == 0 {
                -7
            } else {
                ahead - 7
            }
        }
        Some("next") => {
            if ahead == 0 {
                7
            } else {
                ahead
            }
        }
        Some(_) => ahead,
        // A bare weekday is the nearest one, past or future.
        None => {
            if ahead <= 3 {
                ahead
            } else {
                ahead - 7
            }
        }
    };
    shift(Some(r), delta)
}

fn year_of(caps: &Captures, i: usize) -> Option<i32> {
    let text = caps.get(i)?.as_str();
    let value: i32 = text.parse().ok()?;
    Some(match text.len() {
        2 if value < 70 => 2000 + value,
        2 => 1900 + value,
        _ => value,
    })
}

fn time(hour: u32, minute: u32, second: u32) -> Option<NaiveTime> {
    NaiveTime::from_hms_opt(hour, minute, second)
}

/// Date and time mentions in `text`, in order, with a date and an adjacent time
/// ("3 March 2024 at 14:05", "5pm on Tuesday") merged into one `datetime`.
pub(crate) fn find_dates(
    text: &str,
    reference: Option<NaiveDate>,
    order: DateOrder,
) -> Vec<DateMention> {
    let mut pieces: Vec<Piece> = Vec::new();
    let mut claim = |start: usize, end: usize, date: Option<DatePart>, time: Option<NaiveTime>| {
        if (date.is_some() || time.is_some())
            && !pieces.iter().any(|p| start < p.end && p.start < end)
        {
            pieces.push(Piece {
                start,
                end,
                date,
                time,
            });
        }
    };

    for c in ISO.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        let date = (|| NaiveDate::from_ymd_opt(year_of(&c, 1)?, number(&c, 2)?, number(&c, 3)?))();
        let at = c
            .get(4)
            .and_then(|_| time(number(&c, 4)?, number(&c, 5)?, number(&c, 6).unwrap_or(0)));
        if let Some(date) = date {
            let end = if c.get(4).is_some() && at.is_none() {
                c.get(3).expect("day").end()
            } else {
                m.end()
            };
            claim(m.start(), end, Some(absolute(date)), at);
        }
    }
    for c in DAY_MONTH.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        let part = (|| {
            day_month(
                number(&c, 1)?,
                month_number(&c[2])?,
                year_of(&c, 3),
                reference,
            )
        })();
        claim(m.start(), m.end(), part, None);
    }
    for c in MONTH_DAY.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        let part = (|| {
            day_month(
                number(&c, 2)?,
                month_number(&c[1])?,
                year_of(&c, 3),
                reference,
            )
        })();
        claim(m.start(), m.end(), part, None);
    }
    for c in NUMERIC.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        // `1.2.24` is a version number far more often than a date.
        if &c[2] == "." && c[4].len() != 4 {
            continue;
        }
        let (Some(a), Some(b), Some(year)) = (number(&c, 1), number(&c, 3), year_of(&c, 4)) else {
            continue;
        };
        let (day, month) = match (a > 12, b > 12, order) {
            (true, false, _) | (false, false, DateOrder::DayFirst) => (a, b),
            (false, true, _) | (false, false, DateOrder::MonthFirst) => (b, a),
            (true, true, _) => continue,
        };
        if let Some(date) = NaiveDate::from_ymd_opt(year, month, day) {
            claim(m.start(), m.end(), Some(absolute(date)), None);
        }
    }
    for c in DAY_WORD.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        let days = match c[1].to_ascii_lowercase().as_str() {
            "yesterday" => -1,
            "tomorrow" => 1,
            _ => 0,
        };
        claim(
            m.start(),
            m.end(),
            Some(relative(shift(reference, days))),
            None,
        );
    }
    for c in NAMED_WEEKDAY.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        if let Some(day) = weekday(&c[2]) {
            let date = weekday_date(reference, c.get(1).map(|q| q.as_str()), day);
            claim(m.start(), m.end(), Some(relative(date)), None);
        }
    }
    for c in OFFSET.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        let (amount, unit, sign) = match c.get(1) {
            Some(amount) => (amount.as_str(), &c[2], -1),
            None => (&c[3], &c[4], 1),
        };
        let Some(amount) = count(amount) else {
            continue;
        };
        let per = if unit.eq_ignore_ascii_case("week") {
            7
        } else {
            1
        };
        let date = shift(reference, sign * amount as i64 * per);
        claim(m.start(), m.end(), Some(relative(date)), None);
    }
    for c in TIME_12H.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        let hour = number(&c, 1).unwrap_or(0) % 12
            + if c[4].eq_ignore_ascii_case("p") {
                12
            } else {
                0
            };
        let at = time(hour, number(&c, 2).unwrap_or(0), number(&c, 3).unwrap_or(0));
        claim(m.start(), m.end(), None, at);
    }
    for c in TIME_24H.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        let at = time(
            number(&c, 1).unwrap_or(0),
            number(&c, 2).unwrap_or(0),
            number(&c, 3).unwrap_or(0),
        );
        claim(m.start(), m.end(), None, at);
    }

    pieces.sort_by_key(|p| p.start);
    let mut merged: Vec<Piece> = Vec::new();
    for piece in pieces {
        if let Some(last) = merged.last_mut() {
            let between = &text[last.end..piece.start];
            let joins = match (&last.date, last.time, &piece.date, piece.time) {
                (Some(_), None, None, Some(_)) => DATE_THEN_TIME.is_match(between),
                (None, Some(_), Some(_), None) => TIME_THEN_DATE.is_match(between),
                _ => false,
            };
            if joins {
                last.end = piece.end;
                last.date = last.date.take().or(piece.date);
                last.time = last.time.or(piece.time);
                continue;
            }
        }
        merged.push(piece);
    }

    merged
        .into_iter()
        .map(|p| {
            let clock = p.time.map(|t| t.format("%H:%M:%S").to_string());
            let (kind, iso, relative) = match (p.date, clock) {
                (Some(d), Some(clock)) => (
                    "datetime",
                    d.iso.map(|iso| format!("{iso}T{clock}")),
                    d.relative,
                ),
                (Some(d), None) => ("date", d.iso, d.relative),
                (None, clock) => ("time", clock, false),
            };
            DateMention {
                start: p.start,
                end: p.end,
                kind,
                iso,
                relative,
            }
        })
        .collect()
}

/// The capture date recorded by ExifTool or ffprobe, used to resolve relative
/// mentions. File-system dates are ignored: they record copies, not capture.
pub(crate) fn capture_date(metadata: &anytopdf_core::Metadata) -> Option<NaiveDate> {
    const KEYS: &[&str] = &[
        "DateTimeOriginal",
        "CreationDate",
        "CreateDate",
        "MediaCreateDate",
        "creation_time",
    ];
    KEYS.iter().find_map(|wanted| {
        metadata.iter().find_map(|(key, value)| {
            let suffix = key.rsplit([':', '.']).next().unwrap_or(key);
            if suffix != *wanted {
                return None;
            }
            let digits = value.get(..10)?.replace(':', "-");
            NaiveDate::parse_from_str(&digits, "%Y-%m-%d").ok()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(
        text: &str,
        reference: Option<NaiveDate>,
    ) -> Vec<(&'static str, String, Option<String>)> {
        find_dates(text, reference, DateOrder::DayFirst)
            .into_iter()
            .map(|m| (m.kind, text[m.start..m.end].to_string(), m.iso))
            .collect()
    }

    fn day(y: i32, m: u32, d: u32) -> Option<NaiveDate> {
        NaiveDate::from_ymd_opt(y, m, d)
    }

    fn iso(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn absolute_dates_in_common_forms() {
        assert_eq!(
            found(
                "Due 3 March 2024, shipped March 5th, 2024 and Tuesday, 12 Mar 2024.",
                None
            ),
            [
                ("date", "3 March 2024".into(), iso("2024-03-03")),
                ("date", "March 5th, 2024".into(), iso("2024-03-05")),
                ("date", "Tuesday, 12 Mar 2024".into(), iso("2024-03-12")),
            ]
        );
    }

    #[test]
    fn iso_and_numeric_dates_respect_day_order() {
        assert_eq!(
            found("Log 2024-03-03 14:05 then 03/04/2024 and 25/12/24", None),
            [
                (
                    "datetime",
                    "2024-03-03 14:05".into(),
                    iso("2024-03-03T14:05:00")
                ),
                ("date", "03/04/2024".into(), iso("2024-04-03")),
                ("date", "25/12/24".into(), iso("2024-12-25")),
            ]
        );
        let us = find_dates("03/04/2024 and 12/25/2024", None, DateOrder::MonthFirst);
        let isos: Vec<_> = us.into_iter().map(|m| m.iso.unwrap()).collect();
        assert_eq!(isos, ["2024-03-04", "2024-12-25"]);
    }

    #[test]
    fn relative_dates_resolve_against_the_reference() {
        // 2024-03-06 is a Wednesday.
        let reference = day(2024, 3, 6);
        assert_eq!(
            found(
                "Tuesday at 5pm, last Friday, next Monday, yesterday, 2 weeks ago",
                reference
            ),
            [
                (
                    "datetime",
                    "Tuesday at 5pm".into(),
                    iso("2024-03-05T17:00:00")
                ),
                ("date", "last Friday".into(), iso("2024-03-01")),
                ("date", "next Monday".into(), iso("2024-03-11")),
                ("date", "yesterday".into(), iso("2024-03-05")),
                ("date", "2 weeks ago".into(), iso("2024-02-21")),
            ]
        );
    }

    #[test]
    fn relative_dates_without_reference_stay_unresolved() {
        let mentions = find_dates("see you tomorrow at 9:30 am", None, DateOrder::DayFirst);
        assert_eq!(mentions.len(), 1);
        assert_eq!(mentions[0].kind, "datetime");
        assert_eq!(mentions[0].iso, None);
        assert!(mentions[0].relative);
    }

    #[test]
    fn year_less_dates_use_reference_year_or_iso_year_less_form() {
        assert_eq!(
            found("on 5 June", None),
            [("date", "5 June".into(), iso("--06-05"))]
        );
        assert_eq!(
            found("on 5 June", day(2023, 1, 1)),
            [("date", "5 June".into(), iso("2023-06-05"))]
        );
    }

    #[test]
    fn times_alone_and_noise_that_is_not_a_date() {
        assert_eq!(
            found("Meeting 14:05, call at 5:30 p.m.", None),
            [
                ("time", "14:05".into(), iso("14:05:00")),
                ("time", "5:30 p.m.".into(), iso("17:30:00")),
            ]
        );
        for text in [
            "version 1.2.24",
            "ratio 3:2",
            "31/31/2024",
            "2024-13-40",
            "I may go",
            "page 25:99",
        ] {
            assert!(
                found(text, None).is_empty(),
                "{text}: {:?}",
                found(text, None)
            );
        }
    }

    #[test]
    fn capture_date_prefers_exif_original_over_ffprobe() {
        let mut metadata = anytopdf_core::Metadata::new();
        metadata.insert(
            "ffprobe.format.tags.creation_time".into(),
            "2023-01-02T03:04:05.000000Z".into(),
        );
        assert_eq!(capture_date(&metadata), day(2023, 1, 2));
        metadata.insert(
            "exiftool.ExifIFD:DateTimeOriginal".into(),
            "2024:03:06 10:00:00".into(),
        );
        metadata.insert(
            "exiftool.System:FileModifyDate".into(),
            "2025:01:01 00:00:00".into(),
        );
        assert_eq!(capture_date(&metadata), day(2024, 3, 6));
    }
}
