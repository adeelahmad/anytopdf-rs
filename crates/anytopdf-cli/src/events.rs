use serde_json::{Value, json};
use std::io::Write;

pub(crate) enum RunStatus {
    Ok,
    Partial,
    Failed,
}

pub(crate) enum Event {
    RunStarted {
        profile: &'static str,
        inputs: usize,
    },
    RunFinished {
        status: RunStatus,
        exit_code: u8,
        error: Option<String>,
    },
    StageStarted {
        stage: &'static str,
    },
    StageFinished {
        stage: &'static str,
    },
    SourceStarted {
        index: usize,
        input: String,
    },
    SourceImported {
        index: usize,
        input: String,
        units: usize,
    },
    SourceSkipped {
        index: usize,
        input: String,
        code: String,
    },
    UnitStarted {
        unit: usize,
        source: usize,
        enricher: String,
    },
    UnitFinished {
        unit: usize,
        source: usize,
        enricher: String,
    },
    Diagnostic {
        code: String,
        severity: &'static str,
        message: String,
        input: Option<String>,
    },
    OutputWritten {
        path: String,
        kind: &'static str,
        pages: Option<usize>,
    },
}

pub(crate) struct EventWriter<W: Write> {
    out: W,
    seq: u64,
    broken: bool,
}

impl Event {
    fn name_and_fields(&self) -> (&'static str, Vec<(&'static str, Value)>) {
        match self {
            Event::RunStarted { profile, inputs } => (
                "run.started",
                vec![("profile", json!(profile)), ("inputs", json!(inputs))],
            ),
            Event::RunFinished {
                status,
                exit_code,
                error,
            } => {
                let status = match status {
                    RunStatus::Ok => "ok",
                    RunStatus::Partial => "partial",
                    RunStatus::Failed => "failed",
                };
                let mut fields = vec![("status", json!(status)), ("exit_code", json!(exit_code))];
                if let Some(error) = error {
                    fields.push(("error", json!(error)));
                }
                ("run.finished", fields)
            }
            Event::StageStarted { stage } => ("stage.started", vec![("stage", json!(stage))]),
            Event::StageFinished { stage } => ("stage.finished", vec![("stage", json!(stage))]),
            Event::SourceStarted { index, input } => (
                "source.started",
                vec![("index", json!(index)), ("input", json!(input))],
            ),
            Event::SourceImported {
                index,
                input,
                units,
            } => (
                "source.imported",
                vec![
                    ("index", json!(index)),
                    ("input", json!(input)),
                    ("units", json!(units)),
                ],
            ),
            Event::SourceSkipped { index, input, code } => (
                "source.skipped",
                vec![
                    ("index", json!(index)),
                    ("input", json!(input)),
                    ("code", json!(code)),
                ],
            ),
            Event::UnitStarted {
                unit,
                source,
                enricher,
            } => (
                "unit.started",
                vec![
                    ("unit", json!(unit)),
                    ("source", json!(source)),
                    ("enricher", json!(enricher)),
                ],
            ),
            Event::UnitFinished {
                unit,
                source,
                enricher,
            } => (
                "unit.finished",
                vec![
                    ("unit", json!(unit)),
                    ("source", json!(source)),
                    ("enricher", json!(enricher)),
                ],
            ),
            Event::Diagnostic {
                code,
                severity,
                message,
                input,
            } => {
                let mut fields = vec![
                    ("code", json!(code)),
                    ("severity", json!(severity)),
                    ("message", json!(message)),
                ];
                if let Some(input) = input {
                    fields.push(("input", json!(input)));
                }
                ("diagnostic", fields)
            }
            Event::OutputWritten { path, kind, pages } => {
                let mut fields = vec![("path", json!(path)), ("kind", json!(kind))];
                if let Some(pages) = pages {
                    fields.push(("pages", json!(pages)));
                }
                ("output.written", fields)
            }
        }
    }
}

impl<W: Write> EventWriter<W> {
    pub(crate) fn new(out: W) -> Self {
        Self {
            out,
            seq: 0,
            broken: false,
        }
    }

    pub(crate) fn emit(&mut self, event: &Event) {
        if self.broken {
            return;
        }
        let (name, fields) = event.name_and_fields();
        let mut line = format!(
            "{{\"schema_version\":\"anytopdf.events/1\",\"seq\":{},\"event\":{}",
            self.seq,
            json!(name)
        );
        for (key, value) in fields {
            line.push_str(&format!(",\"{key}\":{value}"));
        }
        line.push_str("}\n");
        if self
            .out
            .write_all(line.as_bytes())
            .and_then(|()| self.out.flush())
            .is_err()
        {
            self.broken = true;
            return;
        }
        self.seq += 1;
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by S3-05-T5 broken-pipe handling")
    )]
    pub(crate) fn is_broken(&self) -> bool {
        self.broken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    fn all_events() -> Vec<Event> {
        vec![
            Event::RunStarted {
                profile: "archive",
                inputs: 2,
            },
            Event::StageStarted { stage: "import" },
            Event::SourceStarted {
                index: 0,
                input: "a.txt".into(),
            },
            Event::SourceImported {
                index: 0,
                input: "a.txt".into(),
                units: 1,
            },
            Event::SourceSkipped {
                index: 1,
                input: "blob.xyz".into(),
                code: "input.unsupported".into(),
            },
            Event::UnitStarted {
                unit: 0,
                source: 0,
                enricher: "ocr".into(),
            },
            Event::UnitFinished {
                unit: 0,
                source: 0,
                enricher: "ocr".into(),
            },
            Event::Diagnostic {
                code: "enrichment.failed".into(),
                severity: "warning",
                message: "no provider".into(),
                input: Some("a.txt".into()),
            },
            Event::OutputWritten {
                path: "out.pdf".into(),
                kind: "pdf",
                pages: Some(3),
            },
            Event::StageFinished { stage: "import" },
            Event::RunFinished {
                status: RunStatus::Partial,
                exit_code: 0,
                error: None,
            },
        ]
    }

    struct SharedBuf(Rc<RefCell<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn emit_all(events: &[Event]) -> String {
        let buf = Rc::new(RefCell::new(Vec::<u8>::new()));
        let mut writer = EventWriter::new(SharedBuf(buf.clone()));
        for event in events {
            writer.emit(event);
        }
        let bytes = buf.borrow().clone();
        String::from_utf8(bytes).unwrap()
    }

    fn pos(line: &str, key: &str) -> usize {
        line.find(&format!("\"{key}\":"))
            .unwrap_or_else(|| panic!("key {key} missing in {line}"))
    }

    #[test]
    fn lines_start_with_schema_version_seq_event_in_that_order() {
        let text = emit_all(&all_events());
        assert!(text.is_empty() || text.ends_with('\n'));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 11, "one line per event, got: {text:?}");
        for (n, line) in lines.iter().enumerate() {
            let prefix =
                format!("{{\"schema_version\":\"anytopdf.events/1\",\"seq\":{n},\"event\":\"");
            assert!(line.starts_with(&prefix), "line {n}: {line}");
            for banned in ["\"time", "\"duration", "\"elapsed", "\"timestamp"] {
                assert!(!line.contains(banned), "line {n} has {banned}: {line}");
            }
        }
        let order: [&[&str]; 11] = [
            &["profile", "inputs"],
            &["stage"],
            &["index", "input"],
            &["index", "input", "units"],
            &["index", "input", "code"],
            &["unit", "source", "enricher"],
            &["unit", "source", "enricher"],
            &["code", "severity", "message", "input"],
            &["path", "kind", "pages"],
            &["stage"],
            &["status", "exit_code"],
        ];
        for (line, keys) in lines.iter().zip(order) {
            let offsets: Vec<usize> = keys.iter().map(|k| pos(line, k)).collect();
            assert!(
                offsets.windows(2).all(|w| w[0] < w[1]),
                "order {keys:?} in {line}"
            );
        }
    }

    #[test]
    fn every_emitted_line_validates_against_the_committed_schema() {
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../schemas/events.schema.json")).unwrap();
        let text = emit_all(&all_events());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 11, "expected 11 emitted lines");
        for line in &lines {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            let errors = anytopdf_core::schema::validate(&schema, &value);
            assert!(errors.is_empty(), "{line}: {errors:?}");
        }
        let mut broken: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        broken.as_object_mut().unwrap().remove("seq");
        assert!(!anytopdf_core::schema::validate(&schema, &broken).is_empty());
    }

    struct BrokenSink(Rc<Cell<usize>>);

    impl Write for BrokenSink {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            self.0.set(self.0.get() + 1);
            Err(std::io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn write_failure_is_ignored_after_the_first_error() {
        let calls = Rc::new(Cell::new(0));
        let mut writer = EventWriter::new(BrokenSink(calls.clone()));
        for event in all_events().iter().take(3) {
            writer.emit(event);
        }
        assert!(writer.is_broken());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn optional_fields_are_omitted_not_null() {
        let events = [
            Event::RunFinished {
                status: RunStatus::Ok,
                exit_code: 0,
                error: None,
            },
            Event::OutputWritten {
                path: "m.json".into(),
                kind: "manifest",
                pages: None,
            },
        ];
        let text = emit_all(&events);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "expected two lines, got {text:?}");
        assert!(!lines[0].contains("\"error\""));
        assert!(!lines[1].contains("\"pages\""));
        assert!(!text.contains("null"));
    }
}
