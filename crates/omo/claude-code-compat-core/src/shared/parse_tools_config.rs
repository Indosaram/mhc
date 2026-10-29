use std::collections::BTreeMap;
use serde_json::Value;

pub fn parse_tools_config(value: Option<&Value>) -> Option<BTreeMap<String, bool>> {
    let items: Vec<&str> = match value? {
        Value::String(text) => text.split(',').collect(),
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Object(_) => return None,
    };
    let result: BTreeMap<_, _> = items.into_iter()
        .map(str::trim).filter(|item| !item.is_empty())
        .map(|item| (item.to_lowercase(), true)).collect();
    (!result.is_empty()).then_some(result)
}
