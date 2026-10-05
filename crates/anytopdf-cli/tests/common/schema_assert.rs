use serde_json::Value;

pub fn assert_valid(name: &str, instance: &Value) {
    let errors = crate::schema_files::validation_errors(name, instance);
    assert!(errors.is_empty(), "{name} schema errors: {errors:?}");
}
