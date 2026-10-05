use maho_core::session_discovery::SessionInfo;
use serde_json::{Value,json};

pub fn build_disk_thread(info: &SessionInfo) -> Value {
    let modified = std::fs::metadata(&info.path).and_then(|metadata|metadata.modified()).map(chrono::DateTime::<chrono::Utc>::from).map(|mtime|mtime.max(info.modified)).unwrap_or(info.modified);
    let preview = if info.first_message.is_empty() || info.first_message == "(no messages)" {None} else {Some(&info.first_message)};
    json!({"id":info.id,"sessionId":info.id,"sessionPath":info.path,"cwd":info.cwd,"createdAt":info.created.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"updatedAt":modified.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"status":{"type":"notLoaded"},"preview":preview,"name":info.name})
}
pub fn compare_threads(left: &Value,right: &Value) -> std::cmp::Ordering {
    let right_time = super::js_semantics::date_parse_ms(right["updatedAt"].as_str().unwrap_or_default()).unwrap_or(0);
    let left_time = super::js_semantics::date_parse_ms(left["updatedAt"].as_str().unwrap_or_default()).unwrap_or(0);
    right_time.cmp(&left_time).then_with(||super::js_semantics::locale_compare(left["id"].as_str().unwrap_or_default(),right["id"].as_str().unwrap_or_default()))
}
pub fn encode_cursor(offset: usize) -> String {super::js_semantics::encode_cursor_number(offset as f64)}
pub fn decode_cursor(cursor: Option<&str>) -> usize {super::js_semantics::decode_cursor_offset(cursor)}
