//! JSON and JSON Lines reading into [`Node`], plus a structural scanner that
//! locates the byte span of any value so record units can cite exact bytes.
use super::{Node, Segment};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::fmt;
use std::ops::Range;

impl<'de> Deserialize<'de> for Node {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct NodeVisitor;

        impl<'de> Visitor<'de> for NodeVisitor {
            type Value = Node;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_unit<E>(self) -> Result<Node, E> {
                Ok(Node::Null)
            }
            fn visit_none<E>(self) -> Result<Node, E> {
                Ok(Node::Null)
            }
            fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Node, D::Error> {
                Node::deserialize(d)
            }
            fn visit_bool<E>(self, v: bool) -> Result<Node, E> {
                Ok(Node::Bool(v))
            }
            fn visit_i64<E>(self, v: i64) -> Result<Node, E> {
                Ok(Node::Number(v.to_string()))
            }
            fn visit_u64<E>(self, v: u64) -> Result<Node, E> {
                Ok(Node::Number(v.to_string()))
            }
            fn visit_f64<E>(self, v: f64) -> Result<Node, E> {
                Ok(Node::Number(
                    serde_json::Number::from_f64(v)
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| v.to_string()),
                ))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Node, E> {
                Ok(Node::String(v.to_owned()))
            }
            fn visit_string<E>(self, v: String) -> Result<Node, E> {
                Ok(Node::String(v))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Node, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element()? {
                    items.push(item);
                }
                Ok(Node::Array(items))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Node, A::Error> {
                let mut members = Vec::new();
                while let Some((k, v)) = map.next_entry::<String, Node>()? {
                    members.push((k, v));
                }
                Ok(Node::Object(members))
            }
        }

        deserializer.deserialize_any(NodeVisitor)
    }
}

const BOM: &[u8] = b"\xEF\xBB\xBF";

fn strip_bom(bytes: &[u8]) -> (usize, &[u8]) {
    match bytes.strip_prefix(BOM) {
        Some(rest) => (BOM.len(), rest),
        None => (0, bytes),
    }
}

/// Parse one complete JSON document.
pub(crate) fn parse_document(bytes: &[u8]) -> serde_json::Result<Node> {
    serde_json::from_slice(strip_bom(bytes).1)
}

/// One non-blank line of a JSON Lines file.
pub(crate) struct Line {
    /// 1-based line number.
    pub number: usize,
    /// Byte span of the line content, without the line terminator.
    pub span: Range<usize>,
    pub value: serde_json::Result<Node>,
}

/// Parse each non-blank line independently, so one bad line never loses the rest.
pub(crate) fn parse_lines(bytes: &[u8]) -> impl Iterator<Item = Line> + '_ {
    let (bom, _) = strip_bom(bytes);
    let mut start = bom;
    let mut number = 0;
    std::iter::from_fn(move || {
        while start < bytes.len() {
            number += 1;
            let end = bytes[start..]
                .iter()
                .position(|b| *b == b'\n')
                .map_or(bytes.len(), |i| start + i);
            let line_start = start;
            start = end + 1;
            let content_end = if end > line_start && bytes[end - 1] == b'\r' {
                end - 1
            } else {
                end
            };
            let content = &bytes[line_start..content_end];
            if content.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            return Some(Line {
                number,
                span: line_start..content_end,
                value: serde_json::from_slice(content),
            });
        }
        None
    })
}

/// Whether a file prefix opens like JSON: it starts with `{` or `[` and either
/// parses or only fails because the prefix was cut short. The first line alone
/// parsing (JSON Lines) also counts.
pub(crate) fn looks_like_json(prefix: &[u8]) -> bool {
    let body = strip_bom(prefix).1;
    let start = skip_ws(body, 0);
    if !matches!(body.get(start), Some(b'{' | b'[')) {
        return false;
    }
    let plausible = |slice: &[u8]| match serde_json::from_slice::<de::IgnoredAny>(slice) {
        Ok(_) => true,
        Err(e) => e.is_eof(),
    };
    let first_line = body[start..].split(|b| *b == b'\n').next().unwrap_or(&[]);
    plausible(&body[start..]) || serde_json::from_slice::<de::IgnoredAny>(first_line).is_ok()
}

/// Whether the first non-blank line is a JSON value of its own (JSON Lines).
pub(crate) fn first_line_is_json(bytes: &[u8]) -> bool {
    parse_lines(bytes).next().is_some_and(|l| l.value.is_ok())
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while bytes.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

fn skip_string(bytes: &[u8], mut i: usize) -> Option<usize> {
    debug_assert_eq!(bytes.get(i), Some(&b'"'));
    i += 1;
    loop {
        match bytes.get(i)? {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
}

/// End (exclusive) of the value starting at `i`, in already-validated JSON.
fn skip_value(bytes: &[u8], i: usize) -> Option<usize> {
    match bytes.get(i)? {
        b'"' => skip_string(bytes, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut j = i;
            loop {
                match bytes.get(j)? {
                    b'"' => {
                        j = skip_string(bytes, j)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(j + 1);
                        }
                    }
                    _ => {}
                }
                j += 1;
            }
        }
        _ => {
            let mut j = i;
            while bytes
                .get(j)
                .is_some_and(|b| !matches!(b, b',' | b']' | b'}') && !b.is_ascii_whitespace())
            {
                j += 1;
            }
            Some(j)
        }
    }
}

/// A member of a container: its key (objects only), where the member starts
/// (the key for objects) and the span of its value.
pub(crate) struct Child {
    pub key: Option<String>,
    pub start: usize,
    pub value: Range<usize>,
}

/// Children of the container whose value spans `span`.
pub(crate) fn children(bytes: &[u8], span: Range<usize>) -> Option<Vec<Child>> {
    let object = match bytes.get(span.start)? {
        b'{' => true,
        b'[' => false,
        _ => return None,
    };
    let close = if object { b'}' } else { b']' };
    let mut out = Vec::new();
    let mut i = skip_ws(bytes, span.start + 1);
    if bytes.get(i) == Some(&close) {
        return Some(out);
    }
    loop {
        let start = i;
        let key = if object {
            let end = skip_string(bytes, i)?;
            let key: String = serde_json::from_slice(&bytes[i..end]).ok()?;
            i = skip_ws(bytes, end);
            (bytes.get(i) == Some(&b':')).then_some(())?;
            i = skip_ws(bytes, i + 1);
            Some(key)
        } else {
            None
        };
        let end = skip_value(bytes, i)?;
        out.push(Child {
            key,
            start,
            value: i..end,
        });
        i = skip_ws(bytes, end);
        match bytes.get(i)? {
            b',' => i = skip_ws(bytes, i + 1),
            b if *b == close => return Some(out),
            _ => return None,
        }
    }
}

/// Span of the whole document's value.
pub(crate) fn root_span(bytes: &[u8]) -> Option<Range<usize>> {
    let start = skip_ws(bytes, strip_bom(bytes).0);
    Some(start..skip_value(bytes, start)?)
}

/// Span of the value at `path`.
pub(crate) fn locate(bytes: &[u8], path: &[Segment]) -> Option<Range<usize>> {
    let mut span = root_span(bytes)?;
    for segment in path {
        let kids = children(bytes, span)?;
        span = match segment {
            Segment::Key(k) => kids.into_iter().find(|c| c.key.as_ref() == Some(k))?,
            Segment::Index(n) => kids.into_iter().nth(*n)?,
        }
        .value;
    }
    Some(span)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_member_order_survive_parsing() {
        let node =
            parse_document(br#"{"z":1.5,"a":-2,"m":1e300,"big":18446744073709551615}"#).unwrap();
        assert_eq!(
            node,
            Node::Object(vec![
                ("z".into(), Node::Number("1.5".into())),
                ("a".into(), Node::Number("-2".into())),
                ("m".into(), Node::Number("1e+300".into())),
                ("big".into(), Node::Number("18446744073709551615".into())),
            ])
        );
    }

    #[test]
    fn lines_report_numbers_spans_and_per_line_errors() {
        let bytes = b"\xEF\xBB\xBF{\"a\":1}\r\n\n  \nnot json\n[2]";
        let lines: Vec<Line> = parse_lines(bytes).collect();
        assert_eq!(lines.len(), 3);
        assert_eq!((lines[0].number, lines[0].span.clone()), (1, 3..10));
        assert_eq!(&bytes[lines[0].span.clone()], b"{\"a\":1}");
        assert!(lines[1].value.is_err());
        assert_eq!(lines[1].number, 4);
        assert_eq!(lines[2].number, 5);
        assert_eq!(&bytes[lines[2].span.clone()], b"[2]");
    }

    #[test]
    fn locate_finds_exact_value_spans_through_strings_with_brackets() {
        let bytes = br#" { "s": "a]}\"[", "data" : [ {"k":"v"} , [1,2], 3 ] } "#;
        let data = locate(bytes, &[Segment::Key("data".into())]).unwrap();
        assert_eq!(&bytes[data.clone()], br#"[ {"k":"v"} , [1,2], 3 ]"#);
        let kids = children(bytes, data).unwrap();
        let spans: Vec<&[u8]> = kids.iter().map(|c| &bytes[c.value.clone()]).collect();
        assert_eq!(spans, [&br#"{"k":"v"}"#[..], b"[1,2]", b"3"]);
        let root = children(bytes, root_span(bytes).unwrap()).unwrap();
        assert_eq!(root[1].key.as_deref(), Some("data"));
        assert!(bytes[root[1].start..].starts_with(b"\"data\""));
        assert_eq!(locate(bytes, &[Segment::Key("missing".into())]), None);
    }

    #[test]
    fn sniffing_accepts_truncated_json_and_json_lines_only() {
        assert!(looks_like_json(b"{\n  \"a\": [1, 2,"));
        assert!(looks_like_json(b"{\"a\":1}\n{\"a\":2}\n"));
        assert!(looks_like_json(b"\xEF\xBB\xBF  [1]"));
        assert!(!looks_like_json(b"[INFO] started"));
        assert!(!looks_like_json(b"hello {}"));
        assert!(!looks_like_json(b"{ not json"));
    }
}
