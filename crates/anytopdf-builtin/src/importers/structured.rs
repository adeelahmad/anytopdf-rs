//! Structured data (JSON, JSON Lines) importer.
//!
//! Records (JSON Lines, a top-level array of objects, or the main array of an
//! API response such as `{"data": [...]}`) become one text unit each, so each
//! record is its own RAG chunk anchored to its exact bytes. Records flow onto
//! shared pages instead of starting a page each. Any other document becomes an
//! indented outline, split by top-level member when it is long.
use crate::structured::{self, Node, Segment, json};
use anyhow::{Context, Result};
use anytopdf_core::*;
use std::fs;
use std::io::Read;
use std::ops::Range;

/// Options for the structured importer. Field names (kebab-case) are the
/// keys of an `[importer.structured]` configuration table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct StructuredOptions {
    /// Split a JSON document's record array into one unit per record. JSON
    /// Lines are always one unit per line.
    pub records: bool,
    /// Let record units share pages instead of starting a page each.
    pub flow: bool,
    /// Split documents whose outline is longer than this many lines into one
    /// unit per top-level member, and only then look for nested record arrays.
    /// 0 never splits.
    pub split_lines: usize,
}

impl Default for StructuredOptions {
    fn default() -> Self {
        Self {
            records: true,
            flow: true,
            split_lines: 60,
        }
    }
}

#[derive(Default)]
pub struct StructuredImporter {
    options: StructuredOptions,
}

impl StructuredImporter {
    pub fn new(options: StructuredOptions) -> Self {
        Self { options }
    }

    fn too_long(&self, outline_lines: usize) -> bool {
        self.options.split_lines > 0 && outline_lines > self.options.split_lines
    }
}

const SNIFF_BYTES: u64 = 8 * 1024;
/// Above the text importer's sniff (100), below any extension match (300), so
/// a `.txt` holding JSON stays text but an unnamed or `.log` JSON file does not.
const JSON_SNIFF: ProbeScore = ProbeScore(250);
const LINES_EXTENSIONS: [&str; 3] = ["jsonl", "ndjson", "jsonlines"];

pub const FORMAT_KEY: &str = "structured.format";
pub const SHAPE_KEY: &str = "structured.shape";
pub const POINTER_KEY: &str = "structured.pointer";

fn extension(source: &SourceRecord) -> String {
    source
        .path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

impl Plugin for StructuredImporter {
    fn descriptor(&self) -> PluginDescriptor {
        PluginDescriptor {
            name: "structured".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            kind: "importer".into(),
            extensions: ["json"]
                .into_iter()
                .chain(LINES_EXTENSIONS)
                .map(String::from)
                .collect(),
            mime_types: vec!["application/json".into(), "application/x-ndjson".into()],
            priority: 50,
        }
    }
}

impl Importer for StructuredImporter {
    fn probe(&self, source: &SourceRecord) -> ProbeScore {
        let ext = extension(source);
        if self.descriptor().extensions.contains(&ext) {
            return ProbeScore::MIME;
        }
        if matches!(ext.as_str(), "txt" | "md") {
            return ProbeScore::NONE;
        }
        let mut prefix = Vec::new();
        let read =
            fs::File::open(&source.path).and_then(|f| f.take(SNIFF_BYTES).read_to_end(&mut prefix));
        if read.is_ok() && json::looks_like_json(&prefix) {
            JSON_SNIFF
        } else {
            ProbeScore::NONE
        }
    }

    fn import(&self, _ctx: &JobContext, mut source: SourceRecord) -> Result<ImportOutcome> {
        let bytes =
            fs::read(&source.path).with_context(|| format!("read {}", source.path.display()))?;
        let name = basename(&source.path);
        let lines_by_name = LINES_EXTENSIONS.contains(&extension(&source).as_str());
        let mut warnings = Vec::new();
        let units = if lines_by_name {
            self.json_lines(&mut source, &bytes, &name, &mut warnings)
        } else {
            match json::parse_document(&bytes) {
                Ok(root) => self.document(&mut source, &bytes, &root),
                Err(_) if json::first_line_is_json(&bytes) => {
                    self.json_lines(&mut source, &bytes, &name, &mut warnings)
                }
                Err(e) => {
                    warnings.push(
                        Diagnostic::new(
                            DiagnosticCode::LossyDecode,
                            format!("{name} is not valid JSON ({e}); imported as plain text"),
                        )
                        .to_string(),
                    );
                    vec![Unit::text(
                        source.id,
                        String::from_utf8_lossy(&bytes).into_owned(),
                    )]
                }
            }
        };
        Ok(ImportOutcome {
            source,
            units,
            warnings,
        })
    }
}

fn byte_anchor(span: &Range<usize>) -> Anchor {
    Anchor::ByteRange {
        start: span.start as u64,
        end: span.end as u64,
    }
}

impl StructuredImporter {
    /// A record unit: a heading naming where it came from, then `path: value` lines.
    fn record_unit(
        &self,
        source: &SourceRecord,
        heading: String,
        body: Vec<String>,
        span: Option<&Range<usize>>,
    ) -> Unit {
        let mut unit = Unit::text(
            source.id,
            std::iter::once(heading)
                .chain(body)
                .collect::<Vec<_>>()
                .join("\n"),
        );
        unit.anchor = span.map(byte_anchor);
        if self.options.flow {
            unit.metadata
                .insert(LAYOUT_FLOW_KEY.into(), LAYOUT_FLOW_CONTINUOUS.into());
        }
        unit
    }

    fn json_lines(
        &self,
        source: &mut SourceRecord,
        bytes: &[u8],
        name: &str,
        warnings: &mut Vec<String>,
    ) -> Vec<Unit> {
        let mut units = Vec::new();
        let mut bad: Vec<(usize, String)> = Vec::new();
        for line in json::parse_lines(bytes) {
            let index = units.len();
            let heading = format!("Record {} (line {})", index + 1, line.number);
            let body = match &line.value {
                Ok(node) => structured::flat_lines(node),
                Err(e) => {
                    bad.push((line.number, e.to_string()));
                    vec![String::from_utf8_lossy(&bytes[line.span.clone()]).into_owned()]
                }
            };
            let mut unit = self.record_unit(source, heading, body, Some(&line.span));
            unit.metadata
                .insert("structured.line".into(), line.number.to_string());
            units.push(unit);
        }
        if let Some((number, error)) = bad.first() {
            warnings.push(
            Diagnostic::new(
                DiagnosticCode::LossyDecode,
                format!(
                    "{} line(s) of {name} are not valid JSON (first: line {number}: {error}); kept as plain text",
                    bad.len()
                ),
            )
            .to_string(),
        );
        }
        source.metadata.insert(FORMAT_KEY.into(), "jsonl".into());
        source.metadata.insert(SHAPE_KEY.into(), "records".into());
        source
            .metadata
            .insert("structured.records".into(), units.len().to_string());
        if units.is_empty() {
            units.push(Unit::text(source.id, String::new()));
        }
        units
    }

    fn document(&self, source: &mut SourceRecord, bytes: &[u8], root: &Node) -> Vec<Unit> {
        source.metadata.insert(FORMAT_KEY.into(), "json".into());
        let outline = structured::outline_lines(root);
        // A root array is always records; a nested one only once the document is
        // too long to read as one outline, so small configs stay whole.
        if let Some(path) = structured::records_path(root)
            && self.options.records
            && (path.is_empty() || self.too_long(outline.len()))
            && let Some(items) = root.get(&path).and_then(Node::records)
        {
            return self.records(source, bytes, root, &path, items);
        }
        source.metadata.insert(SHAPE_KEY.into(), "document".into());
        if let Node::Object(members) = root
            && members.len() > 1
            && self.too_long(outline.len())
        {
            let spans = json::root_span(bytes).and_then(|span| json::children(bytes, span));
            return members
                .iter()
                .enumerate()
                .map(|(i, (key, value))| {
                    let section = Node::Object(vec![(key.clone(), value.clone())]);
                    let mut unit =
                        Unit::text(source.id, structured::outline_lines(&section).join("\n"));
                    let path = [Segment::Key(key.clone())];
                    unit.metadata
                        .insert(POINTER_KEY.into(), structured::pointer(&path));
                    unit.anchor = spans
                        .as_ref()
                        .and_then(|s| s.get(i))
                        .map(|c| byte_anchor(&(c.start..c.value.end)));
                    unit
                })
                .collect();
        }
        vec![Unit::text(source.id, outline.join("\n"))]
    }

    fn records(
        &self,
        source: &mut SourceRecord,
        bytes: &[u8],
        root: &Node,
        path: &[Segment],
        items: &[Node],
    ) -> Vec<Unit> {
        let pointer = structured::pointer(path);
        let label = structured::dotted(path);
        source.metadata.insert(SHAPE_KEY.into(), "records".into());
        source
            .metadata
            .insert("structured.records".into(), items.len().to_string());
        source
            .metadata
            .insert("structured.records-pointer".into(), pointer.clone());
        let spans = json::locate(bytes, path).and_then(|span| json::children(bytes, span));
        let mut units = Vec::new();
        if !path.is_empty() {
            let envelope = structured::envelope(root, path, items.len());
            let mut unit = Unit::text(source.id, structured::outline_lines(&envelope).join("\n"));
            unit.metadata.insert(POINTER_KEY.into(), String::new());
            units.push(unit);
        }
        for (i, item) in items.iter().enumerate() {
            let heading = if label.is_empty() {
                format!("Record {}", i + 1)
            } else {
                format!("Record {} ({label}[{i}])", i + 1)
            };
            let span = spans.as_ref().and_then(|s| s.get(i)).map(|c| &c.value);
            let mut unit = self.record_unit(source, heading, structured::flat_lines(item), span);
            unit.metadata
                .insert(POINTER_KEY.into(), format!("{pointer}/{i}"));
            units.push(unit);
        }
        units
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BuiltinOptions, register_builtins};
    use std::path::Path;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> SourceRecord {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        SourceRecord::new(path)
    }

    fn import(dir: &Path, name: &str, bytes: &[u8]) -> (ImportOutcome, Vec<u8>) {
        let source = write(dir, name, bytes);
        let ctx = JobContext {
            workspace: dir.into(),
            quiet: true,
        };
        (
            StructuredImporter::default().import(&ctx, source).unwrap(),
            bytes.to_vec(),
        )
    }

    fn text(unit: &Unit) -> &str {
        unit.visible_text.as_deref().unwrap()
    }

    fn span<'a>(bytes: &'a [u8], unit: &Unit) -> &'a [u8] {
        match unit.anchor {
            Some(Anchor::ByteRange { start, end }) => &bytes[start as usize..end as usize],
            ref other => panic!("expected byte range, got {other:?}"),
        }
    }

    #[test]
    fn json_lines_become_one_flowing_record_unit_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let input =
            b"{\"id\":1,\"user\":{\"name\":\"Ada\"}}\n\n{\"id\":2,\"tags\":[\"x\",\"y\"]}\r\n";
        let (outcome, bytes) = import(dir.path(), "events.jsonl", input);
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
        assert_eq!(outcome.units.len(), 2);
        assert_eq!(
            text(&outcome.units[0]),
            "Record 1 (line 1)\nid: 1\nuser.name: Ada"
        );
        assert_eq!(
            text(&outcome.units[1]),
            "Record 2 (line 3)\nid: 2\ntags: [x, y]"
        );
        assert_eq!(
            span(&bytes, &outcome.units[1]),
            b"{\"id\":2,\"tags\":[\"x\",\"y\"]}"
        );
        for unit in &outcome.units {
            assert_eq!(
                unit.metadata.get(LAYOUT_FLOW_KEY).map(String::as_str),
                Some(LAYOUT_FLOW_CONTINUOUS)
            );
        }
        assert_eq!(outcome.units[1].metadata["structured.line"], "3");
        assert_eq!(outcome.source.metadata[SHAPE_KEY], "records");
        assert_eq!(outcome.source.metadata[FORMAT_KEY], "jsonl");
        assert_eq!(outcome.source.metadata["structured.records"], "2");
    }

    #[test]
    fn malformed_json_lines_are_kept_as_text_with_one_coded_warning() {
        let dir = tempfile::tempdir().unwrap();
        let input = b"{\"ok\":true}\n{broken\nalso broken\n{\"ok\":false}\n";
        let (outcome, _) = import(dir.path(), "mixed.ndjson", input);
        assert_eq!(outcome.units.len(), 4);
        assert_eq!(text(&outcome.units[1]), "Record 2 (line 2)\n{broken");
        assert_eq!(text(&outcome.units[3]), "Record 4 (line 4)\nok: false");
        assert_eq!(outcome.warnings.len(), 1, "{:?}", outcome.warnings);
        let d = Diagnostic::from_wire(&outcome.warnings[0]);
        assert_eq!(d.code, DiagnosticCode::LossyDecode);
        assert!(
            d.message.starts_with("2 line(s) of mixed.ndjson"),
            "{}",
            d.message
        );
        assert!(d.message.contains("line 2"), "{}", d.message);
    }

    #[test]
    fn top_level_array_of_objects_becomes_records_with_exact_spans() {
        let dir = tempfile::tempdir().unwrap();
        let input = br#"[
  {"name": "a", "n": 1},
  {"name": "b]", "n": 2}
]"#;
        let (outcome, bytes) = import(dir.path(), "rows.json", input);
        assert_eq!(outcome.units.len(), 2);
        assert_eq!(text(&outcome.units[1]), "Record 2\nname: b]\nn: 2");
        assert_eq!(
            span(&bytes, &outcome.units[1]),
            br#"{"name": "b]", "n": 2}"#
        );
        assert_eq!(outcome.units[1].metadata[POINTER_KEY], "/1");
        assert_eq!(outcome.source.metadata[FORMAT_KEY], "json");
    }

    #[test]
    fn api_envelope_keeps_its_metadata_and_splits_the_data_array() {
        let dir = tempfile::tempdir().unwrap();
        let rows: Vec<String> = (1..=40)
            .map(|i| format!(r#"{{"id":"r{i}","n":{i}}}"#))
            .collect();
        let input = format!(r#"{{"page":{{"next":"abc"}},"data":[{}]}}"#, rows.join(","));
        let (outcome, bytes) = import(dir.path(), "api.json", input.as_bytes());
        assert_eq!(outcome.units.len(), 41);
        assert_eq!(
            text(&outcome.units[0]),
            "page:\n  next: abc\ndata: [40 records below]"
        );
        assert!(!outcome.units[0].metadata.contains_key(LAYOUT_FLOW_KEY));
        assert_eq!(text(&outcome.units[3]), "Record 3 (data[2])\nid: r3\nn: 3");
        assert_eq!(outcome.units[3].metadata[POINTER_KEY], "/data/2");
        assert_eq!(span(&bytes, &outcome.units[3]), br#"{"id":"r3","n":3}"#);
        assert_eq!(
            outcome.source.metadata["structured.records-pointer"],
            "/data"
        );

        let (small, _) = import(
            dir.path(),
            "cfg.json",
            br#"{"deps":[{"name":"a"},{"name":"b"}]}"#,
        );
        assert_eq!(small.units.len(), 1);
        assert_eq!(text(&small.units[0]), "deps:\n  - name: a\n  - name: b");
    }

    #[test]
    fn nested_document_becomes_an_outline_and_long_ones_split_by_member() {
        let dir = tempfile::tempdir().unwrap();
        let (short, _) = import(
            dir.path(),
            "cfg.json",
            br#"{"name":"x","opts":{"debug":true}}"#,
        );
        assert_eq!(short.units.len(), 1);
        assert_eq!(text(&short.units[0]), "name: x\nopts:\n  debug: true");
        assert_eq!(short.source.metadata[SHAPE_KEY], "document");

        let long: String = format!(
            r#"{{"head":{{{}}},"tail":{{"k":"v"}}}}"#,
            (0..80)
                .map(|i| format!(r#""f{i}":{i}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let (split, bytes) = import(dir.path(), "big.json", long.as_bytes());
        assert_eq!(split.units.len(), 2);
        assert!(text(&split.units[0]).starts_with("head:\n  f0: 0"));
        assert_eq!(text(&split.units[1]), "tail:\n  k: v");
        assert_eq!(span(&bytes, &split.units[1]), br#""tail":{"k":"v"}"#);
        assert_eq!(split.units[1].metadata[POINTER_KEY], "/tail");
    }

    #[test]
    fn concatenated_json_in_a_json_file_is_read_as_lines() {
        let dir = tempfile::tempdir().unwrap();
        let (outcome, _) = import(dir.path(), "dump.json", b"{\"a\":1}\n{\"a\":2}\n");
        assert_eq!(outcome.units.len(), 2);
        assert_eq!(outcome.source.metadata[FORMAT_KEY], "jsonl");
        assert!(outcome.warnings.is_empty(), "{:?}", outcome.warnings);
    }

    #[test]
    fn invalid_json_falls_back_to_plain_text_with_coded_warning() {
        let dir = tempfile::tempdir().unwrap();
        let (outcome, _) = import(dir.path(), "bad.json", b"{\n  \"a\": oops\n}\n");
        assert_eq!(outcome.units.len(), 1);
        assert_eq!(text(&outcome.units[0]), "{\n  \"a\": oops\n}\n");
        let d = Diagnostic::from_wire(&outcome.warnings[0]);
        assert_eq!(d.code, DiagnosticCode::LossyDecode);
        assert!(
            d.message.contains("bad.json is not valid JSON"),
            "{}",
            d.message
        );
    }

    #[test]
    fn options_parse_from_kebab_case_tables_and_reject_unknown_keys() {
        let parsed: StructuredOptions =
            serde_json::from_str(r#"{"split-lines": 0, "flow": false}"#).unwrap();
        assert_eq!(
            parsed,
            StructuredOptions {
                records: true,
                flow: false,
                split_lines: 0,
            }
        );
        assert!(serde_json::from_str::<StructuredOptions>(r#"{"split_lines": 1}"#).is_err());
        assert_eq!(
            serde_json::from_str::<StructuredOptions>("{}").unwrap(),
            StructuredOptions::default()
        );
    }

    #[test]
    fn options_turn_off_record_splitting_flow_and_member_splitting() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = JobContext {
            workspace: dir.path().into(),
            quiet: true,
        };
        let run = |options: StructuredOptions, name: &str, bytes: &[u8]| {
            StructuredImporter::new(options)
                .import(&ctx, write(dir.path(), name, bytes))
                .unwrap()
        };
        let array = br#"[{"a":1},{"a":2}]"#;
        let whole = run(
            StructuredOptions {
                records: false,
                ..Default::default()
            },
            "rows.json",
            array,
        );
        assert_eq!(whole.units.len(), 1);
        assert_eq!(text(&whole.units[0]), "- a: 1\n- a: 2");

        let paged = run(
            StructuredOptions {
                flow: false,
                ..Default::default()
            },
            "rows.json",
            array,
        );
        assert_eq!(paged.units.len(), 2);
        assert!(
            paged
                .units
                .iter()
                .all(|u| !u.metadata.contains_key(LAYOUT_FLOW_KEY))
        );

        let long = format!(
            r#"{{"a":{{{}}},"b":1}}"#,
            (0..80)
                .map(|i| format!(r#""f{i}":{i}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let never = run(
            StructuredOptions {
                split_lines: 0,
                ..Default::default()
            },
            "big.json",
            long.as_bytes(),
        );
        assert_eq!(never.units.len(), 1);
    }

    #[test]
    fn registry_routes_json_by_extension_and_sniffing_but_keeps_txt_as_text() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = Registry::default();
        register_builtins(&mut registry, BuiltinOptions::default());
        let pick = |name: &str, bytes: &[u8]| {
            registry
                .importer_for(&write(dir.path(), name, bytes))
                .unwrap()
                .descriptor()
                .name
        };
        assert_eq!(pick("a.json", b"{}"), "structured");
        assert_eq!(pick("a.JSONL", b"{}"), "structured");
        assert_eq!(
            pick("app.log", b"{\"level\":\"info\"}\n{\"level\":\"warn\"}\n"),
            "structured"
        );
        assert_eq!(pick("export", b"[{\"a\":1}"), "structured");
        assert_eq!(pick("notes.txt", b"{\"a\":1}"), "text");
        assert_eq!(pick("plain.log", b"[INFO] started\n"), "text");
    }
}
