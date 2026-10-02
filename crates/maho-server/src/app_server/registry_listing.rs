use base64::{Engine,engine::general_purpose::STANDARD};
use maho_core::session_discovery::SessionInfo;
use serde_json::{Value,json};

pub fn build_disk_thread(info: &SessionInfo) -> Value {
    let modified = std::fs::metadata(&info.path).and_then(|metadata|metadata.modified()).map(chrono::DateTime::<chrono::Utc>::from).map(|mtime|mtime.max(info.modified)).unwrap_or(info.modified);
    let preview = if info.first_message.is_empty() || info.first_message == "(no messages)" {None} else {Some(&info.first_message)};
    json!({"id":info.id,"sessionId":info.id,"sessionPath":info.path,"cwd":info.cwd,"createdAt":info.created.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"updatedAt":modified.to_rfc3339_opts(chrono::SecondsFormat::Millis,true),"status":{"type":"notLoaded"},"preview":preview,"name":info.name})
}
pub fn compare_threads(left: &Value,right: &Value) -> std::cmp::Ordering {
    right["updatedAt"].as_str().cmp(&left["updatedAt"].as_str()).then_with(||left["id"].as_str().cmp(&right["id"].as_str()))
}
pub fn encode_cursor(offset: usize) -> String {STANDARD.encode(offset.to_string())}
pub fn decode_cursor(cursor: Option<&str>) -> usize {
    let Some(cursor) = cursor else {return 0};
    let normalized = cursor.chars().filter(|character|character.is_ascii_alphanumeric() || matches!(character,'+'|'/'|'-'|'_')).map(|character|match character {'-'=>'+','_'=>'/',other=>other}).collect::<String>();
    let mut normalized = normalized;
    while normalized.len() % 4 != 0 {normalized.push('=');}
    let Ok(decoded) = STANDARD.decode(normalized) else {return 0};
    let text = String::from_utf8_lossy(&decoded);
    let text = text.trim_start().strip_prefix('+').unwrap_or(text.trim_start());
    let digits = text.chars().take_while(char::is_ascii_digit).collect::<String>();
    digits.parse().unwrap_or(0)
}
