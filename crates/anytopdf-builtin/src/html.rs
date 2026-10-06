//! Dependency-free HTML to plain text conversion.
//!
//! This is not a conforming HTML parser. It keeps the readable text of a page
//! (headings, paragraphs, lists, tables, preformatted blocks and image alt
//! text), drops scripts, styles and markup, and decodes character references.
//! The email importer reuses it for `text/html` bodies.

/// Readable content extracted from an HTML document.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HtmlText {
    pub title: Option<String>,
    pub text: String,
}

/// Elements whose content is never rendered as text.
const SKIPPED: &[&str] = &[
    "script", "style", "template", "noscript", "svg", "math", "iframe", "object",
];

/// Elements that start and end on their own line.
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "body",
    "caption",
    "center",
    "dd",
    "details",
    "dialog",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "header",
    "hr",
    "html",
    "legend",
    "main",
    "nav",
    "ol",
    "p",
    "section",
    "summary",
    "table",
    "tbody",
    "tfoot",
    "thead",
    "ul",
];

/// Like [`html_to_text`], but keeps only the page's main content when it marks one:
/// the single `<main>` element, else the single `<article>`. Navigation, headers and
/// footers outside it are dropped. Returns the element used, if any.
pub fn readable_text(html: &str) -> (HtmlText, Option<&'static str>) {
    let full = html_to_text(html);
    for scope in ["main", "article"] {
        if let Some(inner) = only_element(html, scope) {
            let part = html_to_text(inner);
            if !part.text.trim().is_empty() {
                let text = HtmlText {
                    title: full.title,
                    text: part.text,
                };
                return (text, Some(scope));
            }
        }
    }
    (full, None)
}

/// The content of `name` when the document has exactly one such element.
fn only_element<'a>(html: &'a str, name: &str) -> Option<&'a str> {
    // ASCII lowercasing keeps byte offsets valid for `html`.
    let lower = html.to_ascii_lowercase();
    let open = format!("<{name}");
    let starts: Vec<usize> = lower
        .match_indices(&open)
        .map(|(i, _)| i)
        .filter(|&i| {
            lower[i + open.len()..]
                .chars()
                .next()
                .is_some_and(|c| c == '>' || c == '/' || c.is_ascii_whitespace())
        })
        .collect();
    let [start] = starts.as_slice() else {
        return None;
    };
    let body = start + lower[*start..].find('>')? + 1;
    let end = body + lower[body..].find(&format!("</{name}"))?;
    Some(&html[body..end])
}

pub fn html_to_text(html: &str) -> HtmlText {
    let mut out = Writer::default();
    let mut title: Option<String> = None;
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        out.text(&rest[..lt]);
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            rest = rest.find('>').map_or("", |end| &rest[end + 1..]);
            continue;
        }
        let Some(tag) = parse_tag(rest) else {
            if starts_tag(rest) {
                // A tag cut off by the end of input is dropped, as browsers do.
                rest = "";
                break;
            }
            // A stray `<` that does not open a tag is literal text.
            out.text("<");
            rest = &rest[1..];
            continue;
        };
        rest = &rest[tag.len..];
        let name = tag.name.as_str();
        if tag.closing {
            out.close(name);
            continue;
        }
        if name == "title" && title.is_none() {
            let (inner, after) = raw_content(rest, name);
            let t = collapse(&decode_entities(inner));
            title = (!t.is_empty()).then_some(t);
            rest = after;
            continue;
        }
        if SKIPPED.contains(&name) && !tag.self_closing {
            rest = raw_content(rest, name).1;
            continue;
        }
        out.open(name, &tag.attrs);
    }
    out.text(rest);
    HtmlText {
        title,
        text: out.finish(),
    }
}

struct Tag {
    name: String,
    attrs: String,
    closing: bool,
    self_closing: bool,
    len: usize,
}

fn starts_tag(s: &str) -> bool {
    let body = s[1..].strip_prefix('/').unwrap_or(&s[1..]);
    body.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
}

fn parse_tag(s: &str) -> Option<Tag> {
    let body = &s[1..];
    let (closing, body) = match body.strip_prefix('/') {
        Some(b) => (true, b),
        None => (false, body),
    };
    let name_len = body
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == ':'))
        .unwrap_or(body.len());
    if name_len == 0 || !body.as_bytes()[0].is_ascii_alphabetic() {
        return None;
    }
    let end = find_tag_end(&body[name_len..])?;
    let attrs = &body[name_len..name_len + end];
    Some(Tag {
        name: body[..name_len].to_ascii_lowercase(),
        attrs: attrs.to_string(),
        closing,
        self_closing: attrs.trim_end().ends_with('/'),
        len: 1 + usize::from(closing) + name_len + end + 1,
    })
}

/// Offset of the `>` closing a tag, skipping quoted attribute values.
fn find_tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), _) if c == q => quote = None,
            (None, '>') => return Some(i),
            _ => {}
        }
    }
    None
}

/// Splits raw element content at its closing tag: `(content, rest_after_close)`.
fn raw_content<'a>(s: &'a str, name: &str) -> (&'a str, &'a str) {
    let close = format!("</{name}");
    let lower = s.to_ascii_lowercase();
    match lower.find(&close) {
        Some(at) => {
            let after = &s[at..];
            let end = after.find('>').map_or(after.len(), |i| i + 1);
            (&s[..at], &after[end..])
        }
        None => (s, ""),
    }
}

fn attr(attrs: &str, wanted: &str) -> Option<String> {
    let mut rest = attrs;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() {
            return None;
        }
        let name_end = rest
            .find(|c: char| c.is_whitespace() || c == '=' || c == '/')
            .unwrap_or(rest.len());
        let name = rest[..name_end].to_ascii_lowercase();
        rest = rest[name_end..].trim_start();
        let mut value = String::new();
        if let Some(after) = rest.strip_prefix('=') {
            let after = after.trim_start();
            let (v, remaining) = match after.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let inner = &after[1..];
                    let end = inner.find(q).unwrap_or(inner.len());
                    (&inner[..end], inner.get(end + 1..).unwrap_or(""))
                }
                _ => {
                    let end = after.find(char::is_whitespace).unwrap_or(after.len());
                    (&after[..end], &after[end..])
                }
            };
            value = decode_entities(v);
            rest = remaining;
        }
        if name == wanted {
            return Some(value);
        }
    }
}

#[derive(Default)]
struct Writer {
    out: String,
    pre: usize,
    pending_space: bool,
    list_depth: usize,
    row_cells: usize,
}

impl Writer {
    fn text(&mut self, raw: &str) {
        if raw.is_empty() {
            return;
        }
        let decoded = decode_entities(raw);
        if self.pre > 0 {
            self.out.push_str(&decoded);
            return;
        }
        for c in decoded.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                self.pending_space = true;
            } else {
                if self.pending_space && !self.at_line_start() {
                    self.out.push(' ');
                }
                self.pending_space = false;
                self.out.push(if c == '\u{a0}' { ' ' } else { c });
            }
        }
    }

    fn at_line_start(&self) -> bool {
        self.out.is_empty() || self.out.ends_with('\n') || self.out.ends_with('\t')
    }

    fn newline(&mut self) {
        self.pending_space = false;
        while self.out.ends_with(' ') {
            self.out.pop();
        }
        if !self.out.is_empty() && !self.out.ends_with('\n') {
            self.out.push('\n');
        }
    }

    fn blank_line(&mut self) {
        self.newline();
        if !self.out.is_empty() && !self.out.ends_with("\n\n") {
            self.out.push('\n');
        }
    }

    fn open(&mut self, name: &str, attrs: &str) {
        match name {
            "br" => {
                self.pending_space = false;
                self.out.push('\n');
            }
            "p" | "blockquote" | "table" | "ul" | "ol" | "dl" | "hr" | "figure" => {
                if matches!(name, "ul" | "ol") {
                    self.list_depth += 1;
                }
                if self.list_depth > 1 && matches!(name, "ul" | "ol") {
                    self.newline();
                } else {
                    self.blank_line();
                }
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.blank_line();
                let level = usize::from(name.as_bytes()[1] - b'0');
                self.out.push_str(&"#".repeat(level));
                self.out.push(' ');
            }
            "li" => {
                self.newline();
                self.out
                    .push_str(&"  ".repeat(self.list_depth.saturating_sub(1)));
                self.out.push_str("- ");
            }
            "tr" => {
                self.newline();
                self.row_cells = 0;
            }
            "td" | "th" => {
                if self.row_cells > 0 {
                    self.pending_space = false;
                    self.out.push('\t');
                }
                self.row_cells += 1;
            }
            "pre" => {
                self.blank_line();
                self.pre += 1;
            }
            "img" => {
                if let Some(alt) = attr(attrs, "alt").map(|a| collapse(&a))
                    && !alt.is_empty()
                {
                    self.text(&format!(" [image: {alt}] "));
                }
            }
            _ if BLOCKS.contains(&name) => self.newline(),
            _ => {}
        }
    }

    fn close(&mut self, name: &str) {
        match name {
            "p" | "blockquote" | "table" | "pre" | "figure" | "dl" | "h1" | "h2" | "h3" | "h4"
            | "h5" | "h6" => {
                if name == "pre" {
                    self.pre = self.pre.saturating_sub(1);
                }
                self.blank_line();
            }
            "ul" | "ol" => {
                self.list_depth = self.list_depth.saturating_sub(1);
                if self.list_depth == 0 {
                    self.blank_line();
                } else {
                    self.newline();
                }
            }
            "li" | "tr" => self.newline(),
            _ if BLOCKS.contains(&name) => self.newline(),
            _ => {}
        }
    }

    fn finish(self) -> String {
        let lines: Vec<&str> = self.out.lines().map(str::trim_end).collect();
        let mut text = String::new();
        let mut blank = 0;
        for line in lines {
            if line.is_empty() {
                blank += 1;
                continue;
            }
            if !text.is_empty() {
                text.push_str(if blank > 0 { "\n\n" } else { "\n" });
            }
            blank = 0;
            text.push_str(line);
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text
    }
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let end = rest
            .char_indices()
            .skip(1)
            .take(32)
            .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '#'))
            .map(|(i, _)| i)
            .unwrap_or(rest.len().min(33));
        let name = &rest[1..end];
        match decode_reference(name) {
            Some(c) => {
                out.push(c);
                rest = &rest[end..];
                rest = rest.strip_prefix(';').unwrap_or(rest);
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_reference(name: &str) -> Option<char> {
    if let Some(num) = name.strip_prefix('#') {
        let code = match num.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => num.parse().ok()?,
        };
        return Some(
            char::from_u32(code)
                .filter(|c| *c != '\0')
                .unwrap_or('\u{FFFD}'),
        );
    }
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "shy" => '\u{ad}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "sbquo" => '‚',
        "ldquo" => '“',
        "rdquo" => '”',
        "bdquo" => '„',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "sect" => '§',
        "para" => '¶',
        "iexcl" => '¡',
        "iquest" => '¿',
        "auml" => 'ä',
        "ouml" => 'ö',
        "uuml" => 'ü',
        "Auml" => 'Ä',
        "Ouml" => 'Ö',
        "Uuml" => 'Ü',
        "szlig" => 'ß',
        "eacute" => 'é',
        "egrave" => 'è',
        "agrave" => 'à',
        "aacute" => 'á',
        "ccedil" => 'ç',
        "ntilde" => 'ñ',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "zwnj" => '\u{200c}',
        "zwj" => '\u{200d}',
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_styles_and_head_are_dropped_and_title_is_kept() {
        let out = html_to_text(
            "<!DOCTYPE html><html><head><title> Quarterly  report </title>\
             <style>p{color:red}</style><script>var x = '<p>no</p>';</script></head>\
             <body><!-- hidden --><p>Visible</p></body></html>",
        );
        assert_eq!(out.title.as_deref(), Some("Quarterly report"));
        assert_eq!(out.text, "Visible\n");
    }

    #[test]
    fn blocks_lists_headings_and_tables_keep_their_structure() {
        let out = html_to_text(
            "<h1>Invoice</h1><p>Total   due:\n <b>42</b> EUR</p>\
             <ul><li>one<li>two<ul><li>nested</li></ul></ul>\
             <table><tr><th>Item</th><th>Qty</th></tr><tr><td>Pen</td><td>3</td></tr></table>\
             line<br>break",
        );
        assert_eq!(
            out.text,
            "# Invoice\n\nTotal due: 42 EUR\n\n- one\n- two\n  - nested\n\n\
             Item\tQty\nPen\t3\n\nline\nbreak\n"
        );
    }

    #[test]
    fn entities_and_alt_text_are_decoded() {
        let out = html_to_text(
            "<p>Fish &amp; chips &lt;3 &#233;t&#xE9; caf&eacute;&nbsp;bar &bogus; &copy</p>\
             <img src=\"x.png\" alt=\"A &quot;red&quot; door\"><img alt=''>",
        );
        assert_eq!(
            out.text,
            "Fish & chips <3 été café bar &bogus; ©\n\n[image: A \"red\" door]\n"
        );
    }

    #[test]
    fn preformatted_whitespace_survives() {
        let out = html_to_text("<p>a</p><pre>  x  =  1\n  y</pre><p>b</p>");
        assert_eq!(out.text, "a\n\n  x  =  1\n  y\n\nb\n");
    }

    #[test]
    fn malformed_markup_degrades_to_text() {
        let out = html_to_text("a < b and <3 <p class=\"x>y\">c</p><script>never closed");
        assert_eq!(out.text, "a < b and <3\n\nc\n");
        assert_eq!(html_to_text("<p unterminated").text, "");
        assert_eq!(html_to_text("").text, "");
    }

    #[test]
    fn readable_text_keeps_the_single_main_element_and_the_title() {
        let html = "<html><head><title>Post</title></head><body><nav>Home About</nav>\
                    <MAIN id=x><h1>Post</h1><p>Body text</p></MAIN><footer>(c) site</footer></body></html>";
        let (page, scope) = readable_text(html);
        assert_eq!(scope, Some("main"));
        assert_eq!(page.title.as_deref(), Some("Post"));
        assert!(page.text.contains("Body text"), "{}", page.text);
        assert!(!page.text.contains("Home About"), "{}", page.text);
        assert!(!page.text.contains("(c) site"), "{}", page.text);
    }

    #[test]
    fn readable_text_falls_back_to_the_whole_page() {
        let two = "<article>one</article><article>two</article><nav>menu</nav>";
        let (page, scope) = readable_text(two);
        assert_eq!(scope, None);
        assert!(page.text.contains("menu") && page.text.contains("two"));
        let empty_main = "<main></main><p>outside</p>";
        assert_eq!(readable_text(empty_main).1, None);
        assert_eq!(readable_text("<p>plain <mainly>x</p>").1, None);
    }
}
