use serde_json::Value;
use crate::policy::TRACK_WINDOW;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolCallRecord { pub tool_name: String, pub args_json: String, pub signature: String }
pub fn canonicalize_args(args: Option<&Value>) -> String {
    match args { None | Some(Value::Null) => "{}".into(), Some(value) => stable_stringify(value) }
}
fn stable_stringify(value: &Value) -> String {
    match value {
        Value::Object(record) => {
            let mut keys: Vec<_> = record.keys().collect();
            keys.sort_by_cached_key(|key| key.encode_utf16().collect::<Vec<_>>());
            let parts: Vec<_> = keys.into_iter().map(|key| format!("{}:{}", Value::String(key.clone()), stable_stringify(&record[key]))).collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(stable_stringify).collect::<Vec<_>>().join(",")),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.to_string(),
    }
}
#[derive(Default)]
pub struct ToolCallTracker { calls: Vec<ToolCallRecord> }
impl ToolCallTracker {
    pub fn record(&mut self, tool_name: &str, args: Option<&Value>) -> ToolCallRecord {
        let args_json = canonicalize_args(args);
        let record = ToolCallRecord { tool_name: tool_name.into(), signature: format!("{tool_name}\0{args_json}"), args_json };
        self.calls.push(record.clone());
        if self.calls.len() > TRACK_WINDOW { self.calls.drain(..self.calls.len() - TRACK_WINDOW); }
        record
    }
    pub fn records(&self) -> &[ToolCallRecord] { &self.calls }
    pub fn reset(&mut self) { self.calls.clear(); }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn canonicalization_ignores_key_order() { let a: Value = serde_json::from_str("{\"a\":1,\"b\":{\"d\":2,\"c\":[3]}}").unwrap(); let b: Value = serde_json::from_str("{\"b\":{\"c\":[3],\"d\":2},\"a\":1}").unwrap(); let result = canonicalize_args(Some(&a)); assert_eq!(result, canonicalize_args(Some(&b))); }
    #[test] fn missing_args_default_to_object() { let result = canonicalize_args(None); assert_eq!(result, "{}"); }
    #[test] fn tracker_caps_window() { let mut tracker = ToolCallTracker::default(); for i in 0..100 { tracker.record("bash", Some(&serde_json::json!({"command":format!("cmd {i}")}))); } assert_eq!(tracker.records().len(), 64); }
    #[test] fn reset_clears_records() { let mut tracker = ToolCallTracker::default(); tracker.record("bash", None); tracker.reset(); assert!(tracker.records().is_empty()); }
}
