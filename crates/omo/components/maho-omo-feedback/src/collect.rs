//! Port of `feedback/collect.ts`: structural secret blanking, transcript and engine-log text.

use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::{Map, Value};

pub const TRANSCRIPT_MESSAGE_LIMIT: usize = 50;
pub const TOOL_RESULT_KEEP: usize = 1200;
pub const LOG_WINDOW_MS: i64 = 15 * 60 * 1000;
pub const LOG_LINES_PER_FILE: usize = 200;
pub const LOG_TAIL_BYTES: u64 = 512 * 1024;
pub const LOG_STRING_MAX: usize = 160;
pub const LOG_FIELDS: [&str; 31] = [
    "ts",
    "timestamp",
    "level",
    "event",
    "component",
    "source",
    "kind",
    "type",
    "reason",
    "code",
    "status",
    "outcome",
    "durationMs",
    "elapsedMs",
    "latencyMs",
    "ms",
    "attempt",
    "attempts",
    "turn",
    "from",
    "to",
    "chainKey",
    "model",
    "provider",
    "sessionId",
    "threadId",
    "taskId",
    "requestId",
    "pid",
    "exitCode",
    "signal",
];

const SECRET_CONTAINERS: [&str; 2] = ["headers", "env"];
const REDACTED: &str = "<redacted>";

static SECRET_KEY_WORD: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(^|_)(api_?key|key|keys|token|tokens|access_token|refresh_token|id_token|secret|secrets|password|passwd|passphrase|authorization|auth|cookie|cookies|credential|credentials|private_key|client_secret|bearer|session_key)$",
    )
    .ok()
});
static NOT_SECRET: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(r"(^|_)(max|min|limit|count|budget|window|length|size|context)(_|$)").ok()
});
static CAMEL_BOUNDARY: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"([a-z0-9])([A-Z])").ok());
static USERINFO: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)^([a-z][a-z0-9+.-]*://)[^@/\s]+@").ok());

fn is_secret_key(key: &str) -> bool {
    let (Some(camel), Some(word), Some(not_secret)) = (
        CAMEL_BOUNDARY.as_ref(),
        SECRET_KEY_WORD.as_ref(),
        NOT_SECRET.as_ref(),
    ) else {
        // The patterns are literals; an unbuildable one must fail closed (treat as secret).
        return true;
    };
    let snake = camel
        .replace_all(key, "${1}_${2}")
        .replace('-', "_")
        .to_lowercase();
    word.is_match(&snake) && !not_secret.is_match(&snake)
}

fn blank_userinfo(value: &str) -> String {
    match USERINFO.as_ref() {
        Some(pattern) => pattern.replace(value, "${1}<redacted>@").into_owned(),
        None => REDACTED.to_string(),
    }
}

/// Structural pass before text redaction: values under secret-named keys, and every value inside
/// `headers` and `env` maps, are blanked regardless of what they look like. URL userinfo too.
pub fn blank_config_secrets(value: &Value, inside_secret_container: bool) -> Value {
    match value {
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| blank_config_secrets(item, inside_secret_container))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map {
                if inside_secret_container || is_secret_key(key) {
                    let blanked = if child.is_object() || child.is_array() {
                        blank_config_secrets(child, true)
                    } else {
                        Value::String(REDACTED.into())
                    };
                    out.insert(key.clone(), blanked);
                    continue;
                }
                let container = SECRET_CONTAINERS.contains(&key.to_lowercase().as_str());
                out.insert(key.clone(), blank_config_secrets(child, container));
            }
            Value::Object(out)
        }
        _ if inside_secret_container => Value::String(REDACTED.into()),
        Value::String(text) => Value::String(blank_userinfo(text)),
        other => other.clone(),
    }
}

fn text_of(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    parts
        .iter()
        .filter_map(|part| {
            let piece = match part.get("type").and_then(Value::as_str) {
                Some("text") => part.get("text").map(js_string).unwrap_or_default(),
                Some("toolCall") => {
                    let name = part
                        .get("name")
                        .map(js_string)
                        .unwrap_or_else(|| "tool".into());
                    let arguments = part
                        .get("arguments")
                        .filter(|value| !value.is_null())
                        .cloned()
                        .unwrap_or_else(|| Value::Object(Map::new()));
                    format!("[{name}] {arguments}")
                }
                Some("image") => "[image]".to_string(),
                _ => String::new(),
            };
            (!piece.is_empty()).then_some(piece)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// JS `String(value)` for the JSON values a transcript can hold.
fn js_string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "null".into(),
        other => other.to_string(),
    }
}

fn shorten(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= TOOL_RESULT_KEEP * 2 {
        return text.to_string();
    }
    let cut = chars.len() - TOOL_RESULT_KEEP * 2;
    let head: String = chars[..TOOL_RESULT_KEEP].iter().collect();
    let tail: String = chars[chars.len() - TOOL_RESULT_KEEP..].iter().collect();
    format!("{head} [{cut} characters cut] {tail}")
}

fn clock(timestamp: Option<&Value>) -> String {
    let parsed = match timestamp {
        Some(Value::String(text)) => DateTime::parse_from_rfc3339(text)
            .ok()
            .map(|time| time.with_timezone(&Utc)),
        Some(Value::Number(number)) => number
            .as_i64()
            .and_then(DateTime::<Utc>::from_timestamp_millis),
        _ => None,
    };
    parsed.map_or_else(String::new, |time| {
        format!(" ({})", time.format("%H:%M:%S"))
    })
}

/// The reported session's last messages as plain lines; custom extension entries never appear.
/// Each entry is the raw session entry object (`{type, message, timestamp, ..}`).
pub fn transcript_text(entries: &[Value]) -> String {
    let mut lines = Vec::new();
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let message = entry.get("message");
        let role = message
            .and_then(|message| message.get("role"))
            .map_or_else(|| "unknown".to_string(), js_string);
        let body = shorten(text_of(message.and_then(|message| message.get("content"))).trim())
            .split(['\n'])
            .map(|line| line.strip_suffix('\r').unwrap_or(line))
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n  ");
        lines.push(format!("{role}{}: {body}", clock(entry.get("timestamp"))));
    }
    let start = lines.len().saturating_sub(TRANSCRIPT_MESSAGE_LIMIT);
    lines[start..].join("\n")
}

fn tail_text(path: &Path) -> std::io::Result<String> {
    let bytes = fs::read(path)?;
    let size = bytes.len() as u64;
    let start = size.saturating_sub(LOG_TAIL_BYTES) as usize;
    let text = String::from_utf8_lossy(&bytes[start..]).into_owned();
    if start == 0 {
        return Ok(text);
    }
    Ok(match text.find('\n') {
        Some(index) => text[index + 1..].to_string(),
        None => text,
    })
}

fn project_line(line: &str, since_ms: i64) -> Option<String> {
    let parsed: Value = serde_json::from_str(line).ok()?;
    let object = parsed.as_object()?;
    let stamp = object
        .get("ts")
        .filter(|value| !value.is_null())
        .or_else(|| object.get("timestamp"))
        .map(js_string)
        .unwrap_or_default();
    let ts = DateTime::parse_from_rfc3339(&stamp)
        .ok()?
        .timestamp_millis();
    if ts < since_ms {
        return None;
    }
    let mut kept = Map::new();
    for field in LOG_FIELDS {
        match object.get(field) {
            Some(value @ (Value::Number(_) | Value::Bool(_))) => {
                kept.insert(field.to_string(), value.clone());
            }
            Some(Value::String(text))
                if text.encode_utf16().count() <= LOG_STRING_MAX
                    && !text.contains(['\r', '\n']) =>
            {
                kept.insert(field.to_string(), Value::String(text.clone()));
            }
            _ => {}
        }
    }
    Some(Value::Object(kept).to_string())
}

/// The engine's own logs (`<agent dir>/logs/*.log`) for the last 15 minutes. Each line keeps only
/// allowlisted, content-free fields; the reported conversation has its own section.
pub fn engine_log_text(agent_dir: &Path, now: DateTime<Utc>) -> String {
    let dir = agent_dir.join("logs");
    let Ok(read) = fs::read_dir(&dir) else {
        return String::new();
    };
    let mut names: Vec<String> = read
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.ends_with(".log"))
        .collect();
    names.sort();
    let since = now.timestamp_millis() - LOG_WINDOW_MS;
    let mut blocks = Vec::new();
    for name in names {
        let Ok(text) = tail_text(&dir.join(&name)) else {
            continue;
        };
        let projected: Vec<String> = text
            .split('\n')
            .filter_map(|line| project_line(line, since))
            .collect();
        let start = projected.len().saturating_sub(LOG_LINES_PER_FILE);
        let lines = &projected[start..];
        if !lines.is_empty() {
            blocks.push(format!("## {name}\n{}", lines.join("\n")));
        }
    }
    blocks.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn blanks_secret_keys_containers_and_userinfo() {
        let config = json!({
            "apiKey": "sk-123",
            "maxTokens": 4096,
            "headers": {"x-custom": "abc", "nested": {"deep": "v"}},
            "env": {"HOME": "/h"},
            "url": "https://user:pw@example.com/path",
            "list": [{"password": "p"}],
            "plain": "value"
        });
        let out = blank_config_secrets(&config, false);
        assert_eq!(out["apiKey"], "<redacted>");
        assert_eq!(out["maxTokens"], 4096);
        assert_eq!(out["headers"]["x-custom"], "<redacted>");
        assert_eq!(out["headers"]["nested"]["deep"], "<redacted>");
        assert_eq!(out["env"]["HOME"], "<redacted>");
        assert_eq!(out["url"], "https://<redacted>@example.com/path");
        assert_eq!(out["list"][0]["password"], "<redacted>");
        assert_eq!(out["plain"], "value");
    }

    #[test]
    fn transcript_keeps_messages_only_joins_continuations_and_limits() {
        let mut entries = vec![json!({"type": "custom", "customType": "memory"})];
        entries.push(json!({
            "type": "message", "timestamp": "2026-01-02T03:04:05.000Z",
            "message": {"role": "user", "content": "first\n\nsecond\r\nthird"}
        }));
        entries.push(json!({
            "type": "message",
            "message": {"role": "assistant", "content": [
                {"type": "text", "text": "hi"},
                {"type": "toolCall", "name": "read", "arguments": {"path": "a"}},
                {"type": "image"}
            ]}
        }));
        let text = transcript_text(&entries);
        assert_eq!(
            text,
            "user (03:04:05): first\n  second\n  third\nassistant: hi [read] {\"path\":\"a\"} [image]"
        );

        let many: Vec<Value> = (0..60)
            .map(|i| json!({"type": "message", "message": {"role": "user", "content": format!("m{i}")}}))
            .collect();
        let limited = transcript_text(&many);
        assert_eq!(limited.lines().count(), TRANSCRIPT_MESSAGE_LIMIT);
        assert!(limited.starts_with("user: m10"));
    }

    #[test]
    fn long_bodies_keep_head_and_tail() {
        let body = format!("{}{}", "a".repeat(1200), "b".repeat(1300));
        let shortened = shorten(&body);
        assert!(shortened.contains(" [100 characters cut] "));
        assert!(shortened.starts_with(&"a".repeat(1200)));
        assert!(shortened.ends_with(&"b".repeat(1200)));
    }

    #[test]
    fn engine_logs_keep_allowlisted_recent_scalars() {
        let dir = tempfile::tempdir().expect("tempdir");
        let logs = dir.path().join("logs");
        fs::create_dir_all(&logs).expect("logs dir");
        let now = DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .expect("now")
            .with_timezone(&Utc);
        let lines = [
            json!({"ts": "2026-01-01T11:59:00Z", "level": "info", "message": "secret body", "durationMs": 5, "reason": "multi\nline"}),
            json!({"ts": "2026-01-01T11:00:00Z", "level": "old"}),
            json!({"level": "no-ts"}),
        ];
        let body = lines
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(logs.join("engine.log"), format!("{body}\nnot json\n")).expect("log");
        fs::write(logs.join("engine.log.1"), "x").expect("rotation");
        let text = engine_log_text(dir.path(), now);
        assert_eq!(
            text,
            "## engine.log\n{\"ts\":\"2026-01-01T11:59:00Z\",\"level\":\"info\",\"durationMs\":5}"
        );
        assert_eq!(engine_log_text(&dir.path().join("missing"), now), "");
    }
}
