//! Link lists for `--links`: plain text (one URL per line), browser bookmark exports
//! in the Netscape HTML format (Chrome, Edge, Firefox, Safari) and Chrome's
//! `Bookmarks` JSON file.

use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::HashSet;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) url: String,
    pub(crate) title: Option<String>,
    /// Bookmark folders, outermost first.
    pub(crate) folders: Vec<String>,
}

pub(crate) fn read_links(path: &Path) -> Result<Vec<Link>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.trim_start_matches('\u{feff}');
    let head = text.trim_start();
    let links = if head.starts_with('{') {
        chrome_json(head).with_context(|| format!("parse {}", path.display()))?
    } else if head.starts_with('<') {
        netscape_html(text)
    } else {
        plain_text(text)
    };
    let mut seen = HashSet::new();
    let links: Vec<Link> = links
        .into_iter()
        .filter(|l| is_http(&l.url) && seen.insert(l.url.clone()))
        .collect();
    if links.is_empty() {
        bail!("{} lists no http(s) links", path.display());
    }
    Ok(links)
}

fn is_http(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// One link per line; `#` starts a comment line, and text after the URL is its title.
fn plain_text(text: &str) -> Vec<Link> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            let (url, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let rest = rest.trim();
            is_http(url).then(|| Link {
                url: url.to_string(),
                title: (!rest.is_empty()).then(|| rest.to_string()),
                folders: Vec::new(),
            })
        })
        .collect()
}

/// `<DT><H3>Folder</H3><DL><p> … <DT><A HREF="…">Title</A> … </DL>` nesting.
fn netscape_html(text: &str) -> Vec<Link> {
    let lower = text.to_ascii_lowercase();
    let mut links = Vec::new();
    let mut stack: Vec<String> = Vec::new();
    let mut pending: Option<String> = None;
    let mut at = 0;
    while let Some(off) = lower[at..].find('<') {
        let start = at + off;
        let Some(end) = lower[start..].find('>').map(|e| start + e) else {
            break;
        };
        let tag = &lower[start + 1..end];
        let name: String = tag
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '/')
            .collect();
        at = end + 1;
        match name.as_str() {
            "h3" => {
                let close = lower[at..].find("</h3").map_or(lower.len(), |c| at + c);
                pending = Some(clean(&text[at..close]));
                at = close;
            }
            "dl" => stack.push(pending.take().unwrap_or_default()),
            "/dl" => {
                stack.pop();
            }
            "a" => {
                let close = lower[at..].find("</a").map_or(lower.len(), |c| at + c);
                if let Some(href) = attribute(&text[start + 1..end], "href") {
                    let title = clean(&text[at..close]);
                    links.push(Link {
                        url: decode(&href),
                        title: (!title.is_empty()).then_some(title),
                        folders: stack.iter().filter(|f| !f.is_empty()).cloned().collect(),
                    });
                }
                at = close;
            }
            _ => {}
        }
    }
    links
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(off) = lower[from..].find(name) {
        let i = from + off;
        from = i + name.len();
        let before_ok = i == 0 || lower.as_bytes()[i - 1].is_ascii_whitespace();
        let rest = lower[from..].trim_start();
        if !before_ok || !rest.starts_with('=') {
            continue;
        }
        let value_at = tag.len() - rest.len() + 1;
        let value = tag[value_at..].trim_start();
        return Some(match value.chars().next() {
            Some(q @ ('"' | '\'')) => value[1..].split(q).next().unwrap_or_default().to_string(),
            _ => value
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or_default()
                .to_string(),
        });
    }
    None
}

fn clean(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode(
        out.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .as_str(),
    )
}

fn decode(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

/// Chrome's profile `Bookmarks` file: `roots` of folders holding `url` nodes.
fn chrome_json(text: &str) -> Result<Vec<Link>> {
    let doc: Value = serde_json::from_str(text)?;
    let roots = doc["roots"]
        .as_object()
        .context("not a Chrome Bookmarks file: no `roots`")?;
    let mut links = Vec::new();
    for key in ["bookmark_bar", "other", "synced"] {
        if let Some(node) = roots.get(key) {
            walk(node, &mut Vec::new(), &mut links);
        }
    }
    Ok(links)
}

fn walk(node: &Value, folders: &mut Vec<String>, links: &mut Vec<Link>) {
    let name = node["name"].as_str().unwrap_or_default().trim().to_string();
    match node["type"].as_str() {
        Some("url") => {
            if let Some(url) = node["url"].as_str() {
                links.push(Link {
                    url: url.to_string(),
                    title: (!name.is_empty()).then_some(name),
                    folders: folders.clone(),
                });
            }
        }
        _ => {
            let Some(children) = node["children"].as_array() else {
                return;
            };
            folders.push(name);
            for child in children {
                walk(child, folders, links);
            }
            folders.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(url: &str, title: Option<&str>, folders: &[&str]) -> Link {
        Link {
            url: url.into(),
            title: title.map(String::from),
            folders: folders.iter().map(|f| f.to_string()).collect(),
        }
    }

    fn read(name: &str, body: &str) -> Result<Vec<Link>> {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(name);
        std::fs::write(&path, body).unwrap();
        read_links(&path)
    }

    #[test]
    fn plain_text_lists_skip_comments_duplicates_and_non_http_lines() {
        let links = read(
            "links.txt",
            "# reading list\nhttps://a.example/x  Article A\n\nftp://nope\nhttps://a.example/x\nhttp://b.example\n",
        )
        .unwrap();
        assert_eq!(
            links,
            [
                link("https://a.example/x", Some("Article A"), &[]),
                link("http://b.example", None, &[]),
            ]
        );
    }

    #[test]
    fn netscape_exports_keep_folder_nesting() {
        let html = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<META HTTP-EQUIV="Content-Type" CONTENT="text/html; charset=UTF-8">
<TITLE>Bookmarks</TITLE><H1>Bookmarks</H1>
<DL><p>
    <DT><H3 ADD_DATE="1" PERSONAL_TOOLBAR_FOLDER="true">Bookmarks bar</H3>
    <DL><p>
        <DT><A HREF="https://a.example/?x=1&amp;y=2" ADD_DATE="1">A &amp; B</A>
        <DT><H3>Research</H3>
        <DL><p>
            <DT><A HREF='https://c.example/'>C</A>
            <DT><A HREF="javascript:void(0)">bookmarklet</A>
        </DL><p>
    </DL><p>
    <DT><A HREF="https://top.example/">Top</A>
</DL><p>"#;
        assert_eq!(
            read("bookmarks.html", html).unwrap(),
            [
                link(
                    "https://a.example/?x=1&y=2",
                    Some("A & B"),
                    &["Bookmarks bar"]
                ),
                link(
                    "https://c.example/",
                    Some("C"),
                    &["Bookmarks bar", "Research"]
                ),
                link("https://top.example/", Some("Top"), &[]),
            ]
        );
    }

    #[test]
    fn chrome_bookmarks_json_walks_every_root() {
        let json = r#"{"roots": {
            "bookmark_bar": {"type": "folder", "name": "Bookmarks bar", "children": [
                {"type": "url", "name": "A", "url": "https://a.example/"},
                {"type": "folder", "name": "Research", "children": [
                    {"type": "url", "name": "C", "url": "https://c.example/"}]}]},
            "other": {"type": "folder", "name": "Other bookmarks", "children": [
                {"type": "url", "name": "", "url": "https://o.example/"}]},
            "synced": {"type": "folder", "name": "Mobile bookmarks", "children": []}}}"#;
        assert_eq!(
            read("Bookmarks", json).unwrap(),
            [
                link("https://a.example/", Some("A"), &["Bookmarks bar"]),
                link(
                    "https://c.example/",
                    Some("C"),
                    &["Bookmarks bar", "Research"]
                ),
                link("https://o.example/", None, &["Other bookmarks"]),
            ]
        );
    }

    #[test]
    fn a_list_without_links_is_an_error() {
        assert!(read("empty.txt", "# nothing\n").is_err());
        assert!(read("bad.json", "{\"x\": 1}").is_err());
    }
}
