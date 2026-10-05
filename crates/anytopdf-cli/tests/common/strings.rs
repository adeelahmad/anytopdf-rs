use serde_json::Value;

pub fn collect(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::String(s) => out.push(s.clone()),
        Value::Array(a) => a.iter().for_each(|v| collect(v, out)),
        Value::Object(o) => o.values().for_each(|v| collect(v, out)),
        _ => {}
    }
}
