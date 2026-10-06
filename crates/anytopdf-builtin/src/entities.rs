//! Structured entities from text that other enrichers already produced: URLs,
//! email addresses, domains, app names, and dates and times, from OCR,
//! captions, transcripts and visible text alike.
//!
//! App names come from a small gazetteer, window-title patterns such as
//! `Budget.xlsx - Excel`, and the domains of URLs, never from bare capitalized
//! words. Dates and times become `Timestamp` annotations with a normalized ISO
//! 8601 value; relative ones ("last Friday") resolve against the source's
//! capture date when it is known.

use crate::dates::{DateMention, DateOrder, capture_date, find_dates};
use anyhow::Result;
use anytopdf_core::*;
use chrono::NaiveDate;
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

pub const PROVIDER: &str = "text-entities";
const MAX_ENTITIES_PER_UNIT: usize = 200;

#[derive(Default)]
pub struct EntityEnricher {
    pub date_order: DateOrder,
}

impl Plugin for EntityEnricher {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: PROVIDER.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "unit-enricher".into(),
            extensions: vec![],
            mime_types: vec![],
            priority: -100,
        }
    }
}

impl UnitEnricher for EntityEnricher {
    fn supports(&self, _graph: &DocumentGraph, unit: &Unit) -> bool {
        unit.visible_text
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty())
            || unit
                .annotations
                .iter()
                .any(|a| text_origin(&a.kind).is_some())
    }

    fn enrich_unit(
        &self,
        _ctx: &JobContext,
        graph: &DocumentGraph,
        unit: &mut Unit,
    ) -> Result<Vec<String>> {
        let reference = graph
            .source(unit.source_id)
            .and_then(|s| capture_date(&s.metadata));
        let found = extract(unit, reference, self.date_order);
        unit.annotations.extend(found);
        Ok(vec![])
    }
}

fn text_origin(kind: &AnnotationKind) -> Option<&'static str> {
    match kind {
        AnnotationKind::Ocr => Some("ocr"),
        AnnotationKind::Caption => Some("caption"),
        AnnotationKind::Transcript => Some("transcript"),
        _ => None,
    }
}

/// Where an entity was found, copied onto its annotation.
#[derive(Clone, Copy)]
struct Origin<'a> {
    from: &'static str,
    region: Option<Region>,
    time_range: Option<TimeRange>,
    confidence: Option<f32>,
    via: Option<&'a str>,
}

#[derive(Default)]
struct Found {
    seen: HashSet<(&'static str, String)>,
    out: Vec<Annotation>,
}

impl Found {
    fn add(&mut self, mut annotation: Annotation, entity: &'static str, key: &str, origin: Origin) {
        if self.out.len() >= MAX_ENTITIES_PER_UNIT || !self.seen.insert((entity, dedupe_key(key))) {
            return;
        }
        annotation.region = origin.region;
        annotation.time_range = origin.time_range;
        annotation.confidence = origin.confidence;
        annotation.attributes.insert("entity".into(), entity.into());
        annotation
            .attributes
            .insert("from".into(), origin.from.into());
        if let Some(via) = origin.via {
            annotation.attributes.insert("via".into(), via.into());
        }
        self.out.push(annotation);
    }

    fn push(&mut self, entity: &'static str, value: &str, origin: Origin) {
        let mut annotation = Annotation::text(AnnotationKind::Custom, PROVIDER, value);
        if matches!(entity, "url" | "domain") {
            annotation.attributes.insert("href".into(), href(value));
        }
        self.add(annotation, entity, value, origin);
    }

    fn push_date(&mut self, text: &str, mention: &DateMention, origin: Origin) {
        let matched = &text[mention.start..mention.end];
        let mut annotation = Annotation::text(AnnotationKind::Timestamp, PROVIDER, matched);
        if let Some(iso) = &mention.iso {
            annotation.attributes.insert("iso".into(), iso.clone());
        }
        if mention.relative {
            annotation
                .attributes
                .insert("relative".into(), "true".into());
        }
        let key = mention.iso.as_deref().unwrap_or(matched);
        self.add(annotation, mention.kind, key, origin);
    }

    /// URLs, emails, domains, app names and dates in one piece of text.
    fn scan(
        &mut self,
        text: &str,
        origin: Origin,
        apps: bool,
        dates: Option<(Option<NaiveDate>, DateOrder)>,
    ) {
        for (entity, value) in text_entities(text) {
            self.push(entity, &value, origin);
            if apps && let Some(app) = app_for_address(&value) {
                self.push(
                    "app",
                    app,
                    Origin {
                        via: Some("domain"),
                        ..origin
                    },
                );
            }
        }
        if apps {
            for (app, via) in app_names(text) {
                self.push(
                    "app",
                    app,
                    Origin {
                        via: Some(via),
                        ..origin
                    },
                );
            }
        }
        if let Some((reference, order)) = dates {
            for mention in find_dates(text, reference, order) {
                self.push_date(text, &mention, origin);
            }
        }
    }
}

/// A followable absolute URL: `www.x.com` and bare `x.com/a` gain `https://`,
/// and the scheme and host are lower-cased.
pub(crate) fn href(address: &str) -> String {
    let (scheme, rest) = match address.find("://") {
        Some(i) => (address[..i].to_ascii_lowercase(), &address[i + 3..]),
        None => ("https".to_string(), address),
    };
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    format!(
        "{scheme}://{}{}",
        rest[..end].to_ascii_lowercase(),
        &rest[end..]
    )
}

fn dedupe_key(value: &str) -> String {
    value.trim_end_matches('/').to_lowercase()
}

fn extract(unit: &Unit, reference: Option<NaiveDate>, order: DateOrder) -> Vec<Annotation> {
    let mut found = Found::default();
    let dates = Some((reference, order));
    // Moments in the media: an annotation's own time range, else the unit's
    // (the keyframe a word was read from).
    let origin = |from, a: Option<&Annotation>| Origin {
        from,
        region: a.and_then(|a| a.region),
        time_range: a.and_then(|a| a.time_range).or(unit.time_range),
        confidence: a.and_then(|a| a.confidence),
        via: None,
    };
    if let Some(text) = &unit.visible_text {
        // Document prose mentions apps without showing them; only links and dates count.
        found.scan(text, origin("text", None), false, dates);
    }
    for annotation in &unit.annotations {
        match text_origin(&annotation.kind) {
            // OCR words are scanned for addresses one by one, so each keeps its
            // box; names and dates span words, so they come from rebuilt lines.
            Some("ocr") => found.scan(
                &annotation.text,
                origin("ocr", Some(annotation)),
                true,
                None,
            ),
            Some(from) => found.scan(
                &annotation.text,
                origin(from, Some(annotation)),
                true,
                dates,
            ),
            None => {}
        }
    }
    for line in ocr_lines(&unit.annotations) {
        let line_origin = Origin {
            region: line.region,
            confidence: line.confidence,
            ..origin("ocr", None)
        };
        for (app, via) in app_names(&line.text) {
            found.push(
                "app",
                app,
                Origin {
                    via: Some(via),
                    ..line_origin
                },
            );
        }
        if let Some((reference, order)) = dates {
            for mention in find_dates(&line.text, reference, order) {
                found.push_date(&line.text, &mention, line_origin);
            }
        }
    }
    found.out
}

static URL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\b(?:https?://|www\.)[^\s<>"'`]+"#).unwrap());
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[a-z0-9._%+-]+@(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+[a-z]{2,}\b").unwrap()
});
static DOMAIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.)+([a-z]{2,})\b(/[^\s<>"'`]*)?"#)
        .unwrap()
});

/// Top-level domains accepted for bare `name.tld` mentions. File extensions
/// that double as country codes (`.md`, `.rs`, `.py`, `.sh`, `.so`, `.pl`) are
/// left out so `README.md` or `main.rs` never read as domains.
const BARE_TLDS: &[&str] = &[
    "com", "org", "net", "io", "dev", "app", "ai", "co", "gov", "edu", "info", "biz", "me", "tv",
    "us", "uk", "de", "fr", "es", "it", "nl", "se", "no", "dk", "fi", "ch", "at", "be", "ie", "pt",
    "ca", "au", "nz", "jp", "cn", "kr", "pk", "ae", "sa", "br", "mx", "ar", "za", "eu", "xyz",
    "site", "online", "tech", "cloud", "store", "news", "blog", "page", "link",
];

/// URL, email and domain mentions in `text`, in order of appearance. URLs
/// claim their span first, so an address inside a URL is not reported twice.
pub(crate) fn text_entities(text: &str) -> Vec<(&'static str, String)> {
    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut out: Vec<(usize, &'static str, String)> = Vec::new();
    let overlaps =
        |spans: &[(usize, usize)], s: usize, e: usize| spans.iter().any(|&(a, b)| s < b && a < e);

    for m in URL.find_iter(text) {
        let value = trim_trailing(m.as_str());
        if value.len() > value.find("://").map_or(4, |i| i + 3) {
            spans.push((m.start(), m.start() + value.len()));
            out.push((m.start(), "url", value.to_string()));
        }
    }
    for m in EMAIL.find_iter(text) {
        if !overlaps(&spans, m.start(), m.end()) {
            spans.push((m.start(), m.end()));
            out.push((m.start(), "email", m.as_str().to_string()));
        }
    }
    for c in DOMAIN.captures_iter(text) {
        let m = c.get(0).expect("whole match");
        let tld = c[1].to_ascii_lowercase();
        let preceded_by_word = text[..m.start()]
            .chars()
            .next_back()
            .is_some_and(|ch| matches!(ch, '@' | '.' | '/' | '-' | '_'));
        if !BARE_TLDS.contains(&tld.as_str())
            || preceded_by_word
            || overlaps(&spans, m.start(), m.end())
        {
            continue;
        }
        let value = trim_trailing(m.as_str());
        let entity = if c.get(2).is_some() && value.contains('/') && !value.ends_with('/') {
            "url"
        } else {
            "domain"
        };
        let value = if entity == "domain" {
            value.trim_end_matches('/')
        } else {
            value
        };
        spans.push((m.start(), m.start() + value.len()));
        out.push((m.start(), entity, value.to_string()));
    }
    out.sort_by_key(|(start, _, _)| *start);
    out.into_iter().map(|(_, e, v)| (e, v)).collect()
}

/// Drops sentence punctuation and unbalanced closing brackets after a URL.
fn trim_trailing(mut value: &str) -> &str {
    loop {
        let Some(last) = value.chars().next_back() else {
            return value;
        };
        let unbalanced = |open: char| value.matches(open).count() < value.matches(last).count();
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '\u{2019}' | '\u{201d}' => true,
            ')' => unbalanced('('),
            ']' => unbalanced('['),
            '}' => unbalanced('{'),
            '>' => true,
            _ => false,
        };
        if !drop {
            return value;
        }
        value = &value[..value.len() - last.len_utf8()];
    }
}

/// Lower-case host without `www.`, and the path, of a URL or bare domain.
fn host_and_path(address: &str) -> (String, &str) {
    let rest = address.find("://").map_or(address, |i| &address[i + 3..]);
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let host = rest[..end]
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let host = host
        .strip_prefix("www.")
        .map(str::to_string)
        .unwrap_or(host);
    (host, &rest[end..])
}

/// `(host suffix, path prefix, app)`; the first matching row wins.
const APP_DOMAINS: &[(&str, &str, &str)] = &[
    ("docs.google.com", "/spreadsheets", "Google Sheets"),
    ("docs.google.com", "/presentation", "Google Slides"),
    ("docs.google.com", "/forms", "Google Forms"),
    ("docs.google.com", "", "Google Docs"),
    ("mail.google.com", "", "Gmail"),
    ("drive.google.com", "", "Google Drive"),
    ("meet.google.com", "", "Google Meet"),
    ("calendar.google.com", "", "Google Calendar"),
    ("github.com", "", "GitHub"),
    ("gitlab.com", "", "GitLab"),
    ("bitbucket.org", "", "Bitbucket"),
    ("youtube.com", "", "YouTube"),
    ("youtu.be", "", "YouTube"),
    ("slack.com", "", "Slack"),
    ("zoom.us", "", "Zoom"),
    ("teams.microsoft.com", "", "Microsoft Teams"),
    ("teams.live.com", "", "Microsoft Teams"),
    ("outlook.office.com", "", "Microsoft Outlook"),
    ("outlook.office365.com", "", "Microsoft Outlook"),
    ("outlook.live.com", "", "Microsoft Outlook"),
    ("notion.so", "", "Notion"),
    ("notion.site", "", "Notion"),
    ("figma.com", "", "Figma"),
    ("linear.app", "", "Linear"),
    ("atlassian.net", "/wiki", "Confluence"),
    ("atlassian.net", "", "Jira"),
    ("trello.com", "", "Trello"),
    ("asana.com", "", "Asana"),
    ("chatgpt.com", "", "ChatGPT"),
    ("chat.openai.com", "", "ChatGPT"),
    ("claude.ai", "", "Claude"),
    ("linkedin.com", "", "LinkedIn"),
    ("x.com", "", "X"),
    ("twitter.com", "", "X"),
    ("facebook.com", "", "Facebook"),
    ("instagram.com", "", "Instagram"),
    ("reddit.com", "", "Reddit"),
    ("wikipedia.org", "", "Wikipedia"),
    ("stackoverflow.com", "", "Stack Overflow"),
    ("netflix.com", "", "Netflix"),
    ("spotify.com", "", "Spotify"),
    ("web.whatsapp.com", "", "WhatsApp"),
    ("discord.com", "", "Discord"),
    ("dropbox.com", "", "Dropbox"),
    ("canva.com", "", "Canva"),
    ("miro.com", "", "Miro"),
    ("airtable.com", "", "Airtable"),
    ("lightning.force.com", "", "Salesforce"),
    ("salesforce.com", "", "Salesforce"),
    ("hubspot.com", "", "HubSpot"),
    ("zendesk.com", "", "Zendesk"),
];

fn app_for_address(address: &str) -> Option<&'static str> {
    let (host, path) = host_and_path(address);
    APP_DOMAINS
        .iter()
        .find(|(suffix, prefix, _)| {
            (host == *suffix
                || host
                    .strip_suffix(suffix)
                    .is_some_and(|sub| sub.ends_with('.')))
                && path.starts_with(prefix)
        })
        .map(|(_, _, app)| *app)
}

/// Names that identify an app wherever they appear on screen: `(text, app)`.
const DISTINCTIVE_APPS: &[(&str, &str)] = &[
    ("Visual Studio Code", "Visual Studio Code"),
    ("VS Code", "Visual Studio Code"),
    ("VSCode", "Visual Studio Code"),
    ("Visual Studio", "Visual Studio"),
    ("Xcode", "Xcode"),
    ("IntelliJ IDEA", "IntelliJ IDEA"),
    ("PyCharm", "PyCharm"),
    ("WebStorm", "WebStorm"),
    ("Android Studio", "Android Studio"),
    ("Sublime Text", "Sublime Text"),
    ("iTerm2", "iTerm2"),
    ("Google Chrome", "Google Chrome"),
    ("Firefox", "Firefox"),
    ("Microsoft Edge", "Microsoft Edge"),
    ("Microsoft Word", "Microsoft Word"),
    ("Microsoft Excel", "Microsoft Excel"),
    ("Microsoft PowerPoint", "Microsoft PowerPoint"),
    ("PowerPoint", "Microsoft PowerPoint"),
    ("Microsoft Outlook", "Microsoft Outlook"),
    ("Microsoft Teams", "Microsoft Teams"),
    ("OneNote", "Microsoft OneNote"),
    ("OneDrive", "OneDrive"),
    ("Google Docs", "Google Docs"),
    ("Google Sheets", "Google Sheets"),
    ("Google Slides", "Google Slides"),
    ("Google Drive", "Google Drive"),
    ("Google Meet", "Google Meet"),
    ("Google Calendar", "Google Calendar"),
    ("Gmail", "Gmail"),
    ("Slack", "Slack"),
    ("Zoom Meeting", "Zoom"),
    ("Zoom Workplace", "Zoom"),
    ("WhatsApp", "WhatsApp"),
    ("FaceTime", "FaceTime"),
    ("iMessage", "Messages"),
    ("Skype", "Skype"),
    ("Webex", "Webex"),
    ("YouTube", "YouTube"),
    ("Netflix", "Netflix"),
    ("Spotify", "Spotify"),
    ("GitHub", "GitHub"),
    ("GitLab", "GitLab"),
    ("Bitbucket", "Bitbucket"),
    ("Jira", "Jira"),
    ("Trello", "Trello"),
    ("Figma", "Figma"),
    ("Photoshop", "Adobe Photoshop"),
    ("Adobe Illustrator", "Adobe Illustrator"),
    ("Lightroom", "Adobe Lightroom"),
    ("Premiere Pro", "Adobe Premiere Pro"),
    ("After Effects", "Adobe After Effects"),
    ("Final Cut Pro", "Final Cut Pro"),
    ("QuickTime Player", "QuickTime Player"),
    ("VLC", "VLC"),
    ("Notion", "Notion"),
    ("Evernote", "Evernote"),
    ("Todoist", "Todoist"),
    ("Airtable", "Airtable"),
    ("Dropbox", "Dropbox"),
    ("ChatGPT", "ChatGPT"),
    ("LinkedIn", "LinkedIn"),
    ("Facebook", "Facebook"),
    ("Instagram", "Instagram"),
    ("TikTok", "TikTok"),
    ("Reddit", "Reddit"),
    ("Wikipedia", "Wikipedia"),
    ("Stack Overflow", "Stack Overflow"),
    ("Salesforce", "Salesforce"),
    ("HubSpot", "HubSpot"),
    ("Zendesk", "Zendesk"),
    ("Tableau", "Tableau"),
    ("Power BI", "Power BI"),
    ("Docker Desktop", "Docker Desktop"),
    ("Activity Monitor", "Activity Monitor"),
    ("System Settings", "System Settings"),
    ("System Preferences", "System Preferences"),
    ("Thunderbird", "Thunderbird"),
];

/// Names that are ordinary words elsewhere, so they only count as the first or
/// last part of a window title such as `Budget.xlsx - Excel`: `(text, app)`.
const TITLE_APPS: &[(&str, &str)] = &[
    ("Word", "Microsoft Word"),
    ("Excel", "Microsoft Excel"),
    ("Outlook", "Microsoft Outlook"),
    ("Teams", "Microsoft Teams"),
    ("Code", "Visual Studio Code"),
    ("Chrome", "Google Chrome"),
    ("Safari", "Safari"),
    ("Zoom", "Zoom"),
    ("Terminal", "Terminal"),
    ("iTerm", "iTerm2"),
    ("Finder", "Finder"),
    ("Mail", "Mail"),
    ("Notes", "Notes"),
    ("Messages", "Messages"),
    ("Calendar", "Calendar"),
    ("Photos", "Photos"),
    ("Preview", "Preview"),
    ("Maps", "Maps"),
    ("Music", "Music"),
    ("Keynote", "Keynote"),
    ("Discord", "Discord"),
    ("Telegram", "Telegram"),
    ("Signal", "Signal"),
    ("Postman", "Postman"),
    ("Linear", "Linear"),
    ("Asana", "Asana"),
    ("Confluence", "Confluence"),
    ("Obsidian", "Obsidian"),
    ("Sketch", "Sketch"),
    ("Brave", "Brave"),
    ("Opera", "Opera"),
    ("Arc", "Arc"),
];

const TITLE_SEPARATORS: &[&str] = &[" - ", " \u{2014} ", " \u{2013} ", " | ", " \u{00b7} "];

/// Gazetteer names longest first, so `Visual Studio Code` wins over `Visual Studio`.
static BY_LENGTH: LazyLock<Vec<(&str, &str)>> = LazyLock::new(|| {
    let mut names = DISTINCTIVE_APPS.to_vec();
    names.sort_by_key(|(name, _)| std::cmp::Reverse(name.len()));
    names
});

/// App names in one line of on-screen text, with how each was recognized.
pub(crate) fn app_names(line: &str) -> Vec<(&'static str, &'static str)> {
    let mut out: Vec<(usize, &'static str, &'static str)> = Vec::new();
    let mut taken: Vec<(usize, usize)> = Vec::new();
    for (name, app) in BY_LENGTH.iter() {
        for (start, _) in line.match_indices(name) {
            let end = start + name.len();
            let bounded = !line[..start]
                .chars()
                .next_back()
                .is_some_and(char::is_alphanumeric)
                && !line[end..]
                    .chars()
                    .next()
                    .is_some_and(char::is_alphanumeric);
            if bounded && !taken.iter().any(|&(a, b)| start < b && a < end) {
                taken.push((start, end));
                out.push((start, *app, "name"));
            }
        }
    }
    if let Some(sep) = TITLE_SEPARATORS.iter().find(|sep| line.contains(**sep)) {
        let parts: Vec<&str> = line.split(sep).map(str::trim).collect();
        let ends = [(0, parts[0]), (line.len(), parts[parts.len() - 1])];
        for (position, part) in ends {
            if let Some((_, app)) = TITLE_APPS
                .iter()
                .chain(DISTINCTIVE_APPS)
                .find(|(name, _)| *name == part)
            {
                out.push((position, app, "window-title"));
            }
        }
    }
    out.sort_by_key(|(start, _, _)| *start);
    let mut seen = HashSet::new();
    out.into_iter()
        .filter(|(_, app, _)| seen.insert(*app))
        .map(|(_, app, via)| (app, via))
        .collect()
}

pub(crate) struct Line {
    pub text: String,
    pub region: Option<Region>,
    pub confidence: Option<f32>,
}

/// Rebuilds reading lines from OCR annotations, which are often single words:
/// boxes whose vertical centres are within half a line height join a line, in
/// left-to-right order, and a wide horizontal gap starts a new line.
pub(crate) fn ocr_lines(annotations: &[Annotation]) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut boxed: Vec<(&Annotation, Region)> = Vec::new();
    for a in annotations.iter().filter(|a| a.kind == AnnotationKind::Ocr) {
        match a.region {
            Some(region) => boxed.push((a, region)),
            None => lines.push(Line {
                text: a.text.clone(),
                region: None,
                confidence: a.confidence,
            }),
        }
    }
    let centre = |r: &Region| r.y + r.height / 2.0;
    boxed.sort_by(|a, b| centre(&a.1).total_cmp(&centre(&b.1)));

    let mut rows: Vec<Vec<(&Annotation, Region)>> = Vec::new();
    for word in boxed {
        match rows.last_mut() {
            Some(row)
                if (centre(&word.1) - centre(&row[0].1)).abs()
                    <= row[0].1.height.max(word.1.height) / 2.0 =>
            {
                row.push(word)
            }
            _ => rows.push(vec![word]),
        }
    }
    for mut row in rows {
        row.sort_by(|a, b| a.1.x.total_cmp(&b.1.x));
        let height = row.iter().map(|w| w.1.height).fold(0.0_f32, f32::max);
        let mut current: Vec<(&Annotation, Region)> = Vec::new();
        for word in row {
            if let Some(last) = current.last()
                && word.1.x - (last.1.x + last.1.width) > height * 3.0
            {
                lines.push(join_line(&current));
                current.clear();
            }
            current.push(word);
        }
        if !current.is_empty() {
            lines.push(join_line(&current));
        }
    }
    lines
}

fn join_line(words: &[(&Annotation, Region)]) -> Line {
    let text = words
        .iter()
        .map(|w| w.0.text.trim())
        .collect::<Vec<_>>()
        .join(" ");
    let x = words.iter().map(|w| w.1.x).fold(f32::INFINITY, f32::min);
    let y = words.iter().map(|w| w.1.y).fold(f32::INFINITY, f32::min);
    let right = words
        .iter()
        .map(|w| w.1.x + w.1.width)
        .fold(0.0_f32, f32::max);
    let bottom = words
        .iter()
        .map(|w| w.1.y + w.1.height)
        .fold(0.0_f32, f32::max);
    let confidence = words.iter().filter_map(|w| w.0.confidence).reduce(f32::min);
    Line {
        text,
        region: Some(
            Region {
                x,
                y,
                width: right - x,
                height: bottom - y,
            }
            .clamped(),
        ),
        confidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entities(text: &str) -> Vec<(&'static str, String)> {
        text_entities(text)
    }

    fn word(text: &str, x: f32, y: f32, width: f32) -> Annotation {
        let mut a = Annotation::text(AnnotationKind::Ocr, "tesseract", text);
        a.region = Some(Region {
            x,
            y,
            width,
            height: 0.02,
        });
        a.confidence = Some(0.9);
        a
    }

    fn values<'a>(annotations: &'a [Annotation], entity: &str) -> Vec<&'a str> {
        annotations
            .iter()
            .filter(|a| a.attributes.get("entity").map(String::as_str) == Some(entity))
            .map(|a| a.text.as_str())
            .collect()
    }

    #[test]
    fn urls_drop_trailing_sentence_punctuation_and_unbalanced_brackets() {
        assert_eq!(
            entities(
                "See https://example.com/a?b=1. Or (https://wiki.example.org/X_(y)), www.rust-lang.org!"
            ),
            [
                ("url", "https://example.com/a?b=1".to_string()),
                ("url", "https://wiki.example.org/X_(y)".to_string()),
                ("url", "www.rust-lang.org".to_string()),
            ]
        );
    }

    #[test]
    fn emails_and_bare_domains_are_separate_from_urls() {
        assert_eq!(
            entities(
                "Mail adeel@example.com or visit github.com/adeelahmad/anytopdf-rs and docs.rs."
            ),
            [
                ("email", "adeel@example.com".to_string()),
                ("url", "github.com/adeelahmad/anytopdf-rs".to_string()),
            ]
        );
        assert_eq!(
            entities("Hosted on example.io, see https://example.io/x"),
            [
                ("domain", "example.io".to_string()),
                ("url", "https://example.io/x".to_string()),
            ]
        );
    }

    #[test]
    fn file_names_and_versions_are_not_domains() {
        for text in [
            "README.md",
            "src/main.rs",
            "run.sh",
            "libfoo.so",
            "v1.2.3",
            "e.g. this",
            "report.pdf",
            "Node.js",
        ] {
            assert!(entities(text).is_empty(), "{text}: {:?}", entities(text));
        }
    }

    #[test]
    fn bare_scheme_is_not_a_url() {
        assert!(entities("type http:// then www.").is_empty());
    }

    #[test]
    fn capitalized_words_are_not_app_names() {
        assert!(app_names("Quarterly Report Draft For Review").is_empty());
        assert!(app_names("We will Zoom in on the Word count").is_empty());
    }

    #[test]
    fn gazetteer_prefers_longest_name() {
        assert_eq!(
            app_names("Open in Visual Studio Code or Slack"),
            [("Visual Studio Code", "name"), ("Slack", "name")]
        );
        assert!(app_names("Slackware").is_empty());
    }

    #[test]
    fn window_titles_name_ambiguous_apps() {
        assert_eq!(
            app_names("Budget 2026.xlsx - Excel"),
            [("Microsoft Excel", "window-title")]
        );
        assert_eq!(app_names("Inbox \u{2014} Mail"), [("Mail", "window-title")]);
        assert_eq!(
            app_names("Terminal \u{2014} zsh \u{2014} 80x24"),
            [("Terminal", "window-title")]
        );
        assert!(app_names("Notes from the meeting").is_empty());
    }

    #[test]
    fn domains_map_to_apps() {
        assert_eq!(
            app_for_address("https://docs.google.com/spreadsheets/d/1"),
            Some("Google Sheets")
        );
        assert_eq!(app_for_address("acme.slack.com"), Some("Slack"));
        assert_eq!(
            app_for_address("https://acme.atlassian.net/wiki/x"),
            Some("Confluence")
        );
        assert_eq!(app_for_address("www.github.com/a"), Some("GitHub"));
        assert_eq!(app_for_address("notgithub.com"), None);
    }

    #[test]
    fn ocr_words_regroup_into_lines_split_at_wide_gaps() {
        let annotations = vec![
            word("Excel", 0.36, 0.011, 0.05),
            word("Budget.xlsx", 0.20, 0.010, 0.10),
            word("-", 0.33, 0.012, 0.01),
            word("Other", 0.90, 0.010, 0.05),
            word("Below", 0.20, 0.200, 0.05),
        ];
        let lines: Vec<String> = ocr_lines(&annotations)
            .into_iter()
            .map(|l| l.text)
            .collect();
        assert_eq!(lines, ["Budget.xlsx - Excel", "Other", "Below"]);
    }

    #[test]
    fn ocr_unit_gets_url_domain_app_and_window_title_annotations() {
        let source = SourceRecord::new("frame.png".into());
        let mut unit = Unit::visual(source.id, "frame.png".into());
        unit.annotations = vec![
            word("https://github.com/adeelahmad/anytopdf-rs", 0.1, 0.05, 0.4),
            word("Budget.xlsx", 0.1, 0.01, 0.1),
            word("-", 0.21, 0.01, 0.01),
            word("Excel", 0.23, 0.01, 0.05),
        ];
        let graph = DocumentGraph {
            sources: vec![source],
            units: vec![unit.clone()],
            ..Default::default()
        };
        let ctx = JobContext {
            workspace: std::env::temp_dir(),
            quiet: true,
        };
        assert!(EntityEnricher::default().supports(&graph, &unit));
        EntityEnricher::default()
            .enrich_unit(&ctx, &graph, &mut unit)
            .unwrap();

        let added = &unit.annotations[4..];
        assert_eq!(
            values(added, "url"),
            ["https://github.com/adeelahmad/anytopdf-rs"]
        );
        assert_eq!(values(added, "app"), ["GitHub", "Microsoft Excel"]);
        let url = &added[0];
        assert_eq!(url.kind, AnnotationKind::Custom);
        assert_eq!(url.provider, PROVIDER);
        assert_eq!(url.attributes["from"], "ocr");
        assert_eq!(url.region.unwrap().x, 0.1);
        assert_eq!(added[1].attributes["via"], "domain");
        assert_eq!(added[2].attributes["via"], "window-title");
        let title = added[2].region.unwrap();
        assert!((title.width - 0.18).abs() < 1e-5);
        // The original OCR words are untouched.
        assert_eq!(unit.annotations[0].kind, AnnotationKind::Ocr);
        DocumentGraph {
            sources: graph.sources.clone(),
            units: vec![unit],
            ..Default::default()
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn transcript_entities_keep_time_and_are_deduplicated() {
        let source = SourceRecord::new("talk.vtt".into());
        let mut unit = Unit::text(source.id, "Slides at example.com, shared on Slack".into());
        let mut cue = Annotation::text(
            AnnotationKind::Transcript,
            "whisper",
            "Write to me at me@example.com, slides at EXAMPLE.com",
        );
        cue.time_range = Some(TimeRange {
            start_seconds: 3.0,
            end_seconds: 6.0,
        });
        unit.annotations.push(cue);
        let added = extract(&unit, None, DateOrder::DayFirst);
        assert_eq!(values(&added, "domain"), ["example.com"]);
        assert_eq!(values(&added, "email"), ["me@example.com"]);
        assert!(
            values(&added, "app").is_empty(),
            "visible document text never names apps"
        );
        let email = added
            .iter()
            .find(|a| a.attributes["entity"] == "email")
            .unwrap();
        assert_eq!(email.attributes["from"], "transcript");
        assert_eq!(email.time_range.unwrap().start_seconds, 3.0);
    }

    #[test]
    fn dates_from_ocr_lines_transcripts_and_captions_resolve_against_capture_date() {
        let mut source = SourceRecord::new("meeting.mp4".into());
        // A Wednesday.
        source.metadata.insert(
            "exiftool.QuickTime:CreateDate".into(),
            "2024:03:06 09:00:00".into(),
        );
        let mut frame = Unit::visual(source.id, "frame.png".into());
        frame.time_range = Some(TimeRange::point(750.0));
        frame.annotations = vec![
            word("Due", 0.1, 0.5, 0.05),
            word("3", 0.16, 0.5, 0.01),
            word("March", 0.18, 0.5, 0.06),
            word("2024", 0.25, 0.5, 0.05),
        ];
        let mut cue = Annotation::text(
            AnnotationKind::Transcript,
            "whisper",
            "let's meet last Friday at 5pm on Zoom Meeting",
        );
        cue.time_range = Some(TimeRange {
            start_seconds: 12.0,
            end_seconds: 15.0,
        });
        frame.annotations.push(cue);
        frame.annotations.push(Annotation::text(
            AnnotationKind::Caption,
            "vlm",
            "A Figma window open on a laptop",
        ));
        let graph = DocumentGraph {
            sources: vec![source],
            units: vec![frame.clone()],
            ..Default::default()
        };
        let ctx = JobContext {
            workspace: std::env::temp_dir(),
            quiet: true,
        };
        EntityEnricher::default()
            .enrich_unit(&ctx, &graph, &mut frame)
            .unwrap();
        let stamps: Vec<_> = frame
            .annotations
            .iter()
            .filter(|a| a.kind == AnnotationKind::Timestamp)
            .collect();
        assert_eq!(stamps.len(), 2, "{stamps:?}");

        let spoken = stamps
            .iter()
            .find(|a| a.attributes["from"] == "transcript")
            .unwrap();
        assert_eq!(spoken.text, "last Friday at 5pm");
        assert_eq!(spoken.attributes["entity"], "datetime");
        assert_eq!(spoken.attributes["iso"], "2024-03-01T17:00:00");
        assert_eq!(spoken.attributes["relative"], "true");
        assert_eq!(spoken.time_range.unwrap().start_seconds, 12.0);

        let shown = stamps
            .iter()
            .find(|a| a.attributes["from"] == "ocr")
            .unwrap();
        assert_eq!(shown.text, "3 March 2024");
        assert_eq!(shown.attributes["iso"], "2024-03-03");
        assert!(!shown.attributes.contains_key("relative"));
        assert!(shown.region.is_some());
        assert_eq!(shown.time_range.unwrap().start_seconds, 750.0);

        let apps = values(&frame.annotations, "app");
        assert_eq!(apps, ["Zoom", "Figma"]);
    }

    #[test]
    fn entity_count_per_unit_is_capped() {
        let text: String = (0..500).map(|i| format!("site{i}.com ")).collect();
        let source = SourceRecord::new("many.txt".into());
        let unit = Unit::text(source.id, text);
        assert_eq!(
            extract(&unit, None, DateOrder::DayFirst).len(),
            MAX_ENTITIES_PER_UNIT
        );
    }

    #[test]
    fn urls_and_domains_carry_a_followable_href() {
        assert_eq!(href("www.Example.com/A?b"), "https://www.example.com/A?b");
        assert_eq!(href("HTTP://GitHub.com/X"), "http://github.com/X");
        assert_eq!(href("example.io"), "https://example.io");
        let source = SourceRecord::new("n.txt".into());
        let unit = Unit::text(source.id, "see www.rust-lang.org and a@b.com".into());
        let added = extract(&unit, None, DateOrder::DayFirst);
        assert_eq!(added[0].attributes["href"], "https://www.rust-lang.org");
        assert!(
            !added[1].attributes.contains_key("href"),
            "emails are not links"
        );
    }
}
