use serde_json::Value;

pub fn parse_session_jsonl(raw: &str) -> Result<Value, String> {
    let first_line = raw.split('\n').next().unwrap_or("");
    let header: Value = serde_json::from_str(first_line).map_err(|_| "first line of session file is not valid JSON".to_owned())?;
    if header.get("type").and_then(Value::as_str) != Some("session")
        || header.get("id").and_then(Value::as_str).is_none()
        || header.get("cwd").and_then(Value::as_str).is_none_or(str::is_empty) {
        return Err("session file has no valid session header with a cwd".into());
    }
    Ok(header)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPlatform { Windows, Unix, Unknown }

pub fn detect_session_platform(cwd: &str) -> SessionPlatform {
    let bytes = cwd.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\');
    let msys = bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b'/';
    if drive || msys { SessionPlatform::Windows }
    else if cwd.starts_with('/') { SessionPlatform::Unix }
    else { SessionPlatform::Unknown }
}
