//! Format-neutral model for structured data (JSON today; YAML, TOML or CSV
//! readers only need to produce a [`Node`]): shape detection and the text
//! layouts the structured importer renders.
pub(crate) mod json;

use std::fmt::Write as _;

/// A parsed structured value. Objects keep their members in source order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}

/// One step of a path into a [`Node`].
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Segment {
    Key(String),
    Index(usize),
}

/// RFC 6901 JSON Pointer for `path` (`""` is the root).
pub(crate) fn pointer(path: &[Segment]) -> String {
    let mut out = String::new();
    for segment in path {
        out.push('/');
        match segment {
            Segment::Key(k) => out.push_str(&k.replace('~', "~0").replace('/', "~1")),
            Segment::Index(i) => {
                let _ = write!(out, "{i}");
            }
        }
    }
    out
}

/// Human-readable path such as `data.items[3]`.
pub(crate) fn dotted(path: &[Segment]) -> String {
    let mut out = String::new();
    for segment in path {
        match segment {
            Segment::Key(k) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(k);
            }
            Segment::Index(i) => {
                let _ = write!(out, "[{i}]");
            }
        }
    }
    out
}

impl Node {
    fn is_scalar(&self) -> bool {
        !matches!(self, Node::Array(_) | Node::Object(_))
    }

    fn scalar_text(&self) -> String {
        match self {
            Node::Null => "null".into(),
            Node::Bool(b) => b.to_string(),
            Node::Number(n) => n.clone(),
            Node::String(s) => s.clone(),
            Node::Array(a) if a.is_empty() => "[]".into(),
            Node::Object(o) if o.is_empty() => "{}".into(),
            Node::Array(a) if a.iter().all(Node::is_scalar) => format!(
                "[{}]",
                a.iter()
                    .map(Node::scalar_text)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            _ => String::new(),
        }
    }

    /// Whether this renders on one `key: value` line.
    fn is_inline(&self) -> bool {
        match self {
            Node::Array(a) => a.iter().all(Node::is_scalar),
            Node::Object(o) => o.is_empty(),
            _ => true,
        }
    }

    pub(crate) fn get(&self, path: &[Segment]) -> Option<&Node> {
        let Some((first, rest)) = path.split_first() else {
            return Some(self);
        };
        let child = match (self, first) {
            (Node::Object(members), Segment::Key(k)) => {
                members.iter().find(|(key, _)| key == k).map(|(_, v)| v)?
            }
            (Node::Array(items), Segment::Index(i)) => items.get(*i)?,
            _ => return None,
        };
        child.get(rest)
    }

    /// Elements of a record array: a non-empty array of objects or arrays.
    pub(crate) fn records(&self) -> Option<&[Node]> {
        match self {
            Node::Array(items) if !items.is_empty() && items.iter().all(|n| !n.is_scalar()) => {
                Some(items)
            }
            _ => None,
        }
    }
}

/// Push `head: value`, indenting continuation lines of a multi-line value.
fn push_pair(out: &mut Vec<String>, indent: &str, head: &str, value: &str) {
    let mut lines = value.split('\n');
    let first = lines.next().unwrap_or("");
    out.push(match (head.is_empty(), first.is_empty()) {
        (true, _) => format!("{indent}{first}"),
        (false, true) => format!("{indent}{head}:"),
        (false, false) => format!("{indent}{head}: {first}"),
    });
    for line in lines {
        out.push(format!("{indent}  {line}"));
    }
}

/// One `path: value` line per leaf, so a full key path and its value are
/// searchable together (`user.address.city: Lahore`).
pub(crate) fn flat_lines(node: &Node) -> Vec<String> {
    fn walk(node: &Node, path: &mut Vec<Segment>, out: &mut Vec<String>) {
        match node {
            Node::Object(members) if !members.is_empty() => {
                for (k, v) in members {
                    path.push(Segment::Key(k.clone()));
                    walk(v, path, out);
                    path.pop();
                }
            }
            Node::Array(items) if !node.is_inline() => {
                for (i, v) in items.iter().enumerate() {
                    path.push(Segment::Index(i));
                    walk(v, path, out);
                    path.pop();
                }
            }
            leaf => push_pair(out, "", &dotted(path), &leaf.scalar_text()),
        }
    }
    let mut out = Vec::new();
    walk(node, &mut Vec::new(), &mut out);
    out
}

/// An indented, YAML-like outline of a whole document.
pub(crate) fn outline_lines(node: &Node) -> Vec<String> {
    fn walk(node: &Node, depth: usize, out: &mut Vec<String>) {
        let indent = "  ".repeat(depth);
        match node {
            Node::Object(members) if !members.is_empty() => {
                for (k, v) in members {
                    if v.is_inline() {
                        push_pair(out, &indent, k, &v.scalar_text());
                    } else {
                        out.push(format!("{indent}{k}:"));
                        walk(v, depth + 1, out);
                    }
                }
            }
            Node::Array(items) if !node.is_inline() => {
                for item in items {
                    if item.is_inline() {
                        push_pair(out, &indent, "", &format!("- {}", item.scalar_text()));
                    } else {
                        let start = out.len();
                        walk(item, depth + 1, out);
                        if let Some(first) = out.get_mut(start) {
                            let inner = "  ".repeat(depth + 1);
                            let rest = first.strip_prefix(&inner).unwrap_or(first).to_string();
                            *first = format!("{indent}- {rest}");
                        }
                    }
                }
            }
            leaf => push_pair(out, &indent, "", &leaf.scalar_text()),
        }
    }
    let mut out = Vec::new();
    walk(node, 0, &mut out);
    out
}

/// Minimum length of a nested array before it is treated as the document's
/// records rather than part of its outline.
const NESTED_RECORDS_MIN: usize = 2;

/// Path of the array whose elements are the document's records: the root
/// itself, or the longest record array one or two object levels down (API
/// responses such as `{"data": [...]}` or `{"response": {"items": [...]}}`).
pub(crate) fn records_path(root: &Node) -> Option<Vec<Segment>> {
    if root.records().is_some() {
        return Some(Vec::new());
    }
    let Node::Object(members) = root else {
        return None;
    };
    let mut best: Option<(usize, Vec<Segment>)> = None;
    let mut consider = |path: Vec<Segment>, node: &Node| {
        if let Some(items) = node.records()
            && items.len() >= NESTED_RECORDS_MIN
            && best.as_ref().is_none_or(|(n, _)| items.len() > *n)
        {
            best = Some((items.len(), path));
        }
    };
    for (k, v) in members {
        consider(vec![Segment::Key(k.clone())], v);
        if let Node::Object(inner) = v {
            for (k2, v2) in inner {
                consider(vec![Segment::Key(k.clone()), Segment::Key(k2.clone())], v2);
            }
        }
    }
    best.map(|(_, path)| path)
}

/// `root` with the array at `path` replaced by a short placeholder.
pub(crate) fn envelope(root: &Node, path: &[Segment], count: usize) -> Node {
    let Some((first, rest)) = path.split_first() else {
        return Node::String(format!("[{count} records below]"));
    };
    match (root, first) {
        (Node::Object(members), Segment::Key(key)) => Node::Object(
            members
                .iter()
                .map(|(k, v)| {
                    let v = if k == key {
                        envelope(v, rest, count)
                    } else {
                        v.clone()
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        _ => root.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Node {
        json::parse_document(text.as_bytes()).unwrap()
    }

    #[test]
    fn flat_lines_carry_full_key_paths_in_source_order() {
        let node = parse(
            r#"{"id":7,"user":{"name":"Ada","tags":["a","b"]},"items":[{"sku":"X"}],"note":"two\nlines","empty":{}}"#,
        );
        assert_eq!(
            flat_lines(&node),
            [
                "id: 7",
                "user.name: Ada",
                "user.tags: [a, b]",
                "items[0].sku: X",
                "note: two",
                "  lines",
                "empty: {}",
            ]
        );
    }

    #[test]
    fn outline_indents_nested_documents() {
        let node =
            parse(r#"{"name":"x","deps":[{"name":"serde","v":1},"plain"],"opts":{"a":null}}"#);
        assert_eq!(
            outline_lines(&node),
            [
                "name: x",
                "deps:",
                "  - name: serde",
                "    v: 1",
                "  - plain",
                "opts:",
                "  a: null",
            ]
        );
    }

    #[test]
    fn records_path_finds_root_arrays_and_api_envelopes() {
        assert_eq!(records_path(&parse(r#"[{"a":1}]"#)), Some(vec![]));
        assert_eq!(records_path(&parse(r#"[1,2]"#)), None);
        let api = parse(r#"{"meta":{"page":1},"data":[{"a":1},{"a":2}],"x":[{"b":1}]}"#);
        let path = records_path(&api).unwrap();
        assert_eq!(pointer(&path), "/data");
        assert_eq!(
            outline_lines(&envelope(&api, &path, 2)),
            [
                "meta:",
                "  page: 1",
                "data: [2 records below]",
                "x:",
                "  - b: 1"
            ]
        );
        let nested = parse(r#"{"response":{"items":[{"a":1},{"a":2}]}}"#);
        assert_eq!(dotted(&records_path(&nested).unwrap()), "response.items");
        assert_eq!(records_path(&parse(r#"{"one":[{"a":1}]}"#)), None);
    }

    #[test]
    fn pointer_escapes_tilde_and_slash() {
        let path = [Segment::Key("a/b~c".into()), Segment::Index(3)];
        assert_eq!(pointer(&path), "/a~1b~0c/3");
        assert_eq!(dotted(&path), "a/b~c[3]");
    }
}
