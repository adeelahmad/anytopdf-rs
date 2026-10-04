use std::{collections::BTreeSet, fs, path::PathBuf};

use anytopdf_core::schema;
use serde_json::{Value, json};

const CATALOG: [&str; 11] = [
    "run.started",
    "run.finished",
    "stage.started",
    "stage.finished",
    "source.started",
    "source.imported",
    "source.skipped",
    "unit.started",
    "unit.finished",
    "diagnostic",
    "output.written",
];

fn schema_path() -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    root.ancestors()
        .map(|dir| dir.join("schemas"))
        .find(|candidate| candidate.is_dir())
        .expect("schemas/ directory must exist")
        .join("events.schema.json")
}

fn schema_text() -> String {
    let path = schema_path();
    assert!(path.is_file(), "schemas/events.schema.json must exist");
    fs::read_to_string(path).unwrap()
}

fn load_schema() -> Value {
    serde_json::from_str(&schema_text()).expect("events schema must be JSON")
}

fn resolve<'a>(root: &'a Value, node: &'a Value) -> &'a Value {
    match node.get("$ref").and_then(Value::as_str) {
        Some(reference) => root
            .pointer(reference.trim_start_matches('#'))
            .unwrap_or_else(|| panic!("unresolvable $ref {reference}")),
        None => node,
    }
}

fn has_closed_object(node: &Value) -> bool {
    match node {
        Value::Object(map) => {
            map.get("additionalProperties") == Some(&Value::Bool(false))
                || map.values().any(has_closed_object)
        }
        Value::Array(items) => items.iter().any(has_closed_object),
        _ => false,
    }
}

fn errors(instance: &Value) -> Vec<String> {
    schema::validate(&load_schema(), instance)
        .into_iter()
        .map(|error| format!("{}: {}", error.path, error.message))
        .collect()
}

#[test]
fn events_schema_is_v1_with_one_branch_per_event() {
    let text = schema_text();
    let root = load_schema();
    assert_eq!(
        root["$schema"],
        "https://json-schema.org/draft/2020-12/schema"
    );
    assert!(
        root["$id"]
            .as_str()
            .is_some_and(|id| id.ends_with("/schemas/events/v1")),
        "bad $id: {}",
        root["$id"]
    );
    let branches = root["oneOf"].as_array().expect("top-level oneOf");
    let mut names = BTreeSet::new();
    for branch in branches {
        let branch = resolve(&root, branch);
        let name = branch["properties"]["event"]["const"]
            .as_str()
            .expect("branch event const")
            .to_string();
        let required: BTreeSet<&str> = branch["required"]
            .as_array()
            .expect("branch required")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        for key in ["schema_version", "seq", "event"] {
            assert!(required.contains(key), "{name} must require {key}");
        }
        names.insert(name);
    }
    let expected: BTreeSet<String> = CATALOG.iter().map(|name| name.to_string()).collect();
    assert_eq!(names, expected);
    assert!(text.contains("\"anytopdf.events/1\""));
    assert!(
        !has_closed_object(&root),
        "additionalProperties:false is banned"
    );
}

#[test]
fn catalog_samples_validate_and_mutations_are_rejected() {
    let envelope = |seq: u64, event: &str, mut payload: Value| {
        let mut object = json!({"schema_version": "anytopdf.events/1", "seq": seq, "event": event});
        object
            .as_object_mut()
            .unwrap()
            .append(payload.as_object_mut().unwrap());
        object
    };
    let samples = [
        envelope(0, "run.started", json!({"profile": "archive", "inputs": 2})),
        envelope(1, "stage.started", json!({"stage": "discover"})),
        envelope(2, "stage.finished", json!({"stage": "discover"})),
        envelope(3, "source.started", json!({"index": 0, "input": "a.txt"})),
        envelope(
            4,
            "source.imported",
            json!({"index": 0, "input": "a.txt", "units": 1}),
        ),
        envelope(
            5,
            "source.skipped",
            json!({"index": 1, "input": "blob.xyz", "code": "input.unsupported"}),
        ),
        envelope(6, "unit.started", json!({"index": 0, "unit": "unit-0"})),
        envelope(7, "unit.finished", json!({"index": 0, "unit": "unit-0"})),
        envelope(
            8,
            "diagnostic",
            json!({"code": "enrichment.failed", "severity": "warning", "message": "boom"}),
        ),
        envelope(
            9,
            "output.written",
            json!({"kind": "pdf", "path": "out.pdf", "pages": 2}),
        ),
        envelope(
            10,
            "output.written",
            json!({"kind": "manifest", "path": "m.json"}),
        ),
        envelope(
            11,
            "run.finished",
            json!({"status": "partial", "exit_code": 0}),
        ),
    ];
    for sample in &samples {
        let found = errors(sample);
        assert!(found.is_empty(), "{sample} rejected: {found:?}");
    }

    let started = samples[0].clone();
    let mut no_seq = started.clone();
    no_seq.as_object_mut().unwrap().remove("seq");
    let mut negative = started.clone();
    negative["seq"] = json!(-1);
    let mut wrong_version = started.clone();
    wrong_version["schema_version"] = json!("anytopdf.events/2");
    let mut unknown_event = started.clone();
    unknown_event["event"] = json!("run.paused");
    let mut bad_status = samples[11].clone();
    bad_status["status"] = json!("done");
    let mut bad_stage = samples[1].clone();
    bad_stage["stage"] = json!("publish");
    let mut no_code = samples[5].clone();
    no_code.as_object_mut().unwrap().remove("code");
    for (label, mutated) in [
        ("seq missing", no_seq),
        ("seq -1", negative),
        ("schema_version /2", wrong_version),
        ("unknown event", unknown_event),
        ("bad status", bad_status),
        ("bad stage", bad_stage),
        ("skipped without code", no_code),
    ] {
        assert!(!errors(&mutated).is_empty(), "{label} must be rejected");
    }
}
