use serde_json::Value;

pub fn single_document(stdout: &[u8]) -> Value {
    let documents: Vec<Value> = serde_json::Deserializer::from_slice(stdout)
        .into_iter::<Value>()
        .map(|item| item.expect("stdout must be JSON only"))
        .collect();
    assert_eq!(documents.len(), 1, "stdout must hold exactly one document");
    documents.into_iter().next().unwrap()
}
