use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaError {
    pub path: String,
    pub message: String,
}

const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "$defs",
];

pub fn validate(schema: &Value, instance: &Value) -> Vec<SchemaError> {
    let mut errors = Vec::new();
    check(schema, schema, instance, "", &mut errors);
    errors
}

fn err(errors: &mut Vec<SchemaError>, path: &str, message: impl Into<String>) {
    errors.push(SchemaError {
        path: path.to_string(),
        message: message.into(),
    });
}

fn type_matches(name: &str, v: &Value) -> bool {
    match name {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        _ => false,
    }
}

fn check(root: &Value, schema: &Value, inst: &Value, path: &str, errors: &mut Vec<SchemaError>) {
    let Some(map) = schema.as_object() else {
        err(errors, path, "schema must be an object");
        return;
    };
    for (key, val) in map {
        match key.as_str() {
            k if ANNOTATIONS.contains(&k) => {}
            "type" => {
                let names: Vec<&str> = match val {
                    Value::String(s) => vec![s.as_str()],
                    Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                    _ => vec![],
                };
                if !names.iter().any(|n| type_matches(n, inst)) {
                    err(errors, path, format!("expected type {val}"));
                }
            }
            "required" => {
                if let (Some(obj), Some(req)) = (inst.as_object(), val.as_array()) {
                    for name in req.iter().filter_map(Value::as_str) {
                        if !obj.contains_key(name) {
                            err(errors, path, format!("missing required property `{name}`"));
                        }
                    }
                }
            }
            "properties" => {
                if let (Some(obj), Some(props)) = (inst.as_object(), val.as_object()) {
                    for (name, sub) in props {
                        if let Some(child) = obj.get(name) {
                            check(
                                root,
                                sub,
                                child,
                                &format!("{path}/{}", escape(name)),
                                errors,
                            );
                        }
                    }
                }
            }
            "additionalProperties" => {
                if let Some(obj) = inst.as_object() {
                    let known = map.get("properties").and_then(Value::as_object);
                    for (name, child) in obj {
                        if known.is_some_and(|k| k.contains_key(name)) {
                            continue;
                        }
                        let child_path = format!("{path}/{}", escape(name));
                        match val {
                            Value::Bool(false) => err(
                                errors,
                                &child_path,
                                format!("additional property `{name}` is not allowed"),
                            ),
                            Value::Bool(true) => {}
                            sub => check(root, sub, child, &child_path, errors),
                        }
                    }
                }
            }
            "items" => {
                if let Some(arr) = inst.as_array() {
                    for (i, child) in arr.iter().enumerate() {
                        check(root, val, child, &format!("{path}/{i}"), errors);
                    }
                }
            }
            "enum" => {
                if !val.as_array().is_some_and(|a| a.contains(inst)) {
                    err(errors, path, "value is not in enum");
                }
            }
            "const" => {
                if val != inst {
                    err(errors, path, format!("expected const {val}"));
                }
            }
            "minimum" => match (inst.as_f64(), val.as_f64()) {
                (Some(n), Some(min)) if n >= min => {}
                (Some(n), Some(min)) => err(errors, path, format!("{n} is below minimum {min}")),
                _ => err(errors, path, "minimum requires a number"),
            },
            "minItems" => match (inst.as_array(), val.as_u64()) {
                (Some(arr), Some(min)) if arr.len() as u64 >= min => {}
                (Some(_), Some(min)) => err(errors, path, format!("fewer than {min} items")),
                _ => err(errors, path, "minItems requires an array"),
            },
            "anyOf" | "oneOf" => {
                let matches = val
                    .as_array()
                    .map(|subs| {
                        subs.iter()
                            .filter(|s| {
                                let mut e = Vec::new();
                                check(root, s, inst, path, &mut e);
                                e.is_empty()
                            })
                            .count()
                    })
                    .unwrap_or(0);
                let ok = if key == "anyOf" {
                    matches >= 1
                } else {
                    matches == 1
                };
                if !ok {
                    err(errors, path, format!("{key} matched {matches} subschemas"));
                }
            }
            "$ref" => match val.as_str().and_then(|r| r.strip_prefix('#')) {
                Some(pointer) => match root.pointer(pointer) {
                    Some(target) => check(root, target, inst, path, errors),
                    None => err(errors, path, format!("unresolved $ref {val}")),
                },
                None => err(errors, path, format!("unsupported $ref {val}")),
            },
            other => err(errors, path, format!("unsupported keyword `{other}`")),
        }
    }
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths(errors: &[SchemaError]) -> Vec<&str> {
        errors.iter().map(|e| e.path.as_str()).collect()
    }

    #[test]
    fn accepts_conforming_instance_for_supported_keywords() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "title": "doc",
            "type": "object",
            "required": ["name", "kind", "version", "count", "pages"],
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string"},
                "kind": {"enum": ["a", "b"]},
                "version": {"const": "anytopdf.x/1"},
                "count": {"type": "integer", "minimum": 0},
                "pages": {"type": "array", "minItems": 1, "items": {"$ref": "#/$defs/page"}}
            },
            "$defs": {
                "page": {"type": "object", "required": ["n"], "properties": {"n": {"type": "integer"}}}
            }
        });
        let instance = json!({
            "name": "x", "kind": "a", "version": "anytopdf.x/1", "count": 3,
            "pages": [{"n": 1}, {"n": 2}]
        });
        assert_eq!(validate(&schema, &instance), vec![]);
    }

    #[test]
    fn rejects_wrong_type_missing_required_and_extra_properties() {
        let schema = json!({
            "type": "object",
            "required": ["name", "id"],
            "additionalProperties": false,
            "properties": {"name": {"type": "string"}, "id": {"type": "string"}}
        });
        let wrong_type = validate(&schema, &json!({"name": 5, "id": "i"}));
        assert!(!wrong_type.is_empty());
        assert!(paths(&wrong_type).contains(&"/name"), "{wrong_type:?}");

        let missing = validate(&schema, &json!({"name": "n"}));
        assert!(!missing.is_empty());
        assert!(paths(&missing).contains(&""), "{missing:?}");

        let extra = validate(&schema, &json!({"name": "n", "id": "i", "extra": 1}));
        assert!(!extra.is_empty());
        assert!(paths(&extra).contains(&"/extra"), "{extra:?}");
    }

    #[test]
    fn rejects_enum_const_minimum_and_min_items_violations() {
        assert!(!validate(&json!({"enum": ["a", "b"]}), &json!("c")).is_empty());
        assert!(!validate(&json!({"const": "x"}), &json!("y")).is_empty());
        assert!(!validate(&json!({"minimum": 0}), &json!(-1)).is_empty());
        assert!(!validate(&json!({"minItems": 1}), &json!([])).is_empty());
        let int = json!({"type": "integer"});
        assert!(!validate(&int, &json!(1.5)).is_empty());
        assert_eq!(validate(&int, &json!(2)), vec![]);
    }

    #[test]
    fn resolves_local_refs_and_any_of_one_of() {
        let refs = json!({
            "type": "array",
            "items": {"$ref": "#/$defs/page"},
            "$defs": {"page": {"type": "object", "required": ["n"], "properties": {"n": {"type": "integer"}}}}
        });
        assert_eq!(validate(&refs, &json!([{"n": 1}])), vec![]);
        assert!(!validate(&refs, &json!([{"n": "x"}])).is_empty());

        let any_of = json!({"anyOf": [{"type": "string"}, {"type": "integer"}]});
        assert_eq!(validate(&any_of, &json!(1)), vec![]);
        assert!(!validate(&any_of, &json!(true)).is_empty());

        let one_of = json!({"oneOf": [{"type": "integer"}, {"minimum": 0}]});
        assert!(!validate(&one_of, &json!("s")).is_empty());
        assert!(
            !validate(&one_of, &json!(5)).is_empty(),
            "two matches must fail"
        );
        assert_eq!(
            validate(&one_of, &json!(-5)),
            vec![],
            "exactly one match passes"
        );
    }

    #[test]
    fn fails_closed_on_unsupported_keyword_or_remote_ref() {
        for schema in [
            json!({"pattern": "^a"}),
            json!({"$ref": "https://example.com/x.json"}),
            json!({"$ref": "#/$defs/missing"}),
        ] {
            assert!(!validate(&schema, &json!("abc")).is_empty(), "{schema}");
            assert!(!validate(&schema, &json!(1)).is_empty(), "{schema}");
        }
        let annotations = json!({
            "title": "t", "description": "d", "$id": "https://x/y", "$schema": "s",
            "$comment": "c", "default": 1, "examples": [1]
        });
        assert_eq!(validate(&annotations, &json!({"anything": 1})), vec![]);
    }

    #[test]
    fn reports_json_pointer_paths_for_nested_errors() {
        let schema = json!({
            "type": "object",
            "properties": {"sources": {"type": "array", "items": {
                "type": "object", "properties": {"sha256": {"type": "string"}}
            }}}
        });
        let errors = validate(&schema, &json!({"sources": [{"sha256": 1}]}));
        assert!(paths(&errors).contains(&"/sources/0/sha256"), "{errors:?}");
    }
}
