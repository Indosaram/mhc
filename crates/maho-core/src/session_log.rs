//! Port of senpi packages/coding-agent/src/core/session-log.ts.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use chrono::{SecondsFormat, Utc};
use regex::Regex;

use crate::brand::env_value;
use crate::config::{app_name, current_env};

const DEFAULT_MAX_BYTES: u64 = 5 * 1024 * 1024;
const MAX_STRING_LENGTH: usize = 200;

fn blocked_key() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:__proto__|constructor|prototype|headers?|env(?:ironment)?|authorization|credential(?:s)?|password|secret|token|api_?key|client_?secret)$")
            .expect("blocked key regex")
    })
}

fn allowed_data_key() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(?:action|attemptId|disposition|stage|error|mode|count|willRetry|deferAdmission|delivered|restored|cause|accepted|skipped|rejectionCause|reason|durationMs|kind|retryable|phase|op|bytes|generation|requestId|tokens|tokensBefore|tokensAfter|contextWindow|attempt|aborted)$")
            .expect("allowed data key regex")
    })
}

fn sensitive_text() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)((?:authorization\s*[:=]\s*(?:bearer|basic)\s+)|(?:bearer\s+)|(?:[?&](?:api[_-]?key|token|secret|password|auth(?:orization)?)=))[^\s&,"'}\]]+"#)
            .expect("sensitive text regex")
    })
}

fn serialize_value(value: &serde_json::Value) -> Option<serde_json::Value> {
    match value {
        serde_json::Value::String(text) => Some(serde_json::Value::String(safe_text(text))),
        serde_json::Value::Number(_) | serde_json::Value::Bool(_) | serde_json::Value::Null => Some(value.clone()),
        _ => None,
    }
}

fn safe_text(value: &str) -> String {
    let redacted = sensitive_text().replace_all(value, "$1[redacted]").into_owned();
    if redacted.chars().count() <= MAX_STRING_LENGTH {
        redacted
    } else {
        let head: String = redacted.chars().take(MAX_STRING_LENGTH - 3).collect();
        format!("{head}...")
    }
}

fn valid_max_bytes(value: Option<u64>) -> u64 {
    match value {
        Some(value) if value > 0 => value,
        _ => DEFAULT_MAX_BYTES,
    }
}

fn format_line(level: &str, event: &str, data: Option<&serde_json::Map<String, serde_json::Value>>) -> String {
    let mut entry = serde_json::Map::new();
    entry.insert("ts".into(), serde_json::Value::String(Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)));
    entry.insert("level".into(), serde_json::Value::String(level.to_owned()));
    entry.insert("event".into(), serde_json::Value::String(safe_text(event)));
    if let Some(data) = data {
        for (key, value) in data {
            if allowed_data_key().is_match(key) && !blocked_key().is_match(key)
                && let Some(safe) = serialize_value(value)
            {
                entry.insert(key.clone(), safe);
            }
        }
    }
    serde_json::Value::Object(entry).to_string()
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

fn file_exceeds_cap(file_path: &Path, incoming_bytes: u64, max_bytes: u64) -> bool {
    match std::fs::metadata(file_path) {
        Ok(metadata) => metadata.len() + incoming_bytes > max_bytes,
        Err(_) => false,
    }
}

fn write_line(file_path: &Path, line: &str, max_bytes: u64) -> std::io::Result<()> {
    if let Some(parent) = file_path.parent() {
        std::fs::create_dir_all(parent)?;
        set_mode(parent, 0o700);
    }
    let text = format!("{line}\n");
    if file_exceeds_cap(file_path, text.len() as u64, max_bytes) {
        let rotated = PathBuf::from(format!("{}.1", file_path.display()));
        let _ = std::fs::remove_file(&rotated);
        let _ = std::fs::rename(file_path, &rotated);
        set_mode(&rotated, 0o600);
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(file_path)?;
    file.write_all(text.as_bytes())?;
    drop(file);
    set_mode(file_path, 0o600);
    Ok(())
}

pub type LogSink = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Default)]
pub struct SessionLoggerOptions {
    pub sink: Option<LogSink>,
    pub mirror_to_stderr: Option<bool>,
    pub max_bytes: Option<u64>,
}

#[derive(Clone, Default)]
pub struct SessionLogger {
    inner: Option<Arc<SessionLoggerInner>>,
}

struct SessionLoggerInner {
    file_path: PathBuf,
    max_bytes: u64,
    mirror_to_stderr: bool,
    options: SessionLoggerOptions,
    reported_write_failure: Mutex<bool>,
}

impl SessionLogger {
    /// senpi returns a no-op logger when no agent dir is configured.
    pub fn create(agent_dir: Option<&str>, options: SessionLoggerOptions) -> Self {
        let Some(agent_dir) = agent_dir.filter(|dir| !dir.is_empty()) else {
            return Self { inner: None };
        };
        let file_path = Path::new(agent_dir).join("logs").join("session.log");
        let mirror_to_stderr = options
            .mirror_to_stderr
            .unwrap_or_else(|| env_value("SESSION_DEBUG", &current_env()).as_deref() == Some("1"));
        Self {
            inner: Some(Arc::new(SessionLoggerInner {
                file_path,
                max_bytes: valid_max_bytes(options.max_bytes),
                mirror_to_stderr,
                options,
                reported_write_failure: Mutex::new(false),
            })),
        }
    }

    pub fn debug(&self, event: &str, data: Option<&serde_json::Map<String, serde_json::Value>>) {
        self.log("debug", event, data);
    }

    pub fn info(&self, event: &str, data: Option<&serde_json::Map<String, serde_json::Value>>) {
        self.log("info", event, data);
    }

    pub fn warn(&self, event: &str, data: Option<&serde_json::Map<String, serde_json::Value>>) {
        self.log("warn", event, data);
    }

    fn log(&self, level: &str, event: &str, data: Option<&serde_json::Map<String, serde_json::Value>>) {
        let Some(inner) = &self.inner else { return };
        let line = format_line(level, event, data);
        if let Some(sink) = &inner.options.sink {
            sink(&line);
        }
        if let Err(error) = write_line(&inner.file_path, &line, inner.max_bytes) {
            let mut reported = inner.reported_write_failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !*reported {
                *reported = true;
                eprintln!("Unable to write session log {error}");
            }
            return;
        }
        if inner.mirror_to_stderr {
            eprintln!("[{}] {line}", app_name());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn data(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn a_missing_agent_dir_yields_a_noop_logger() {
        SessionLogger::create(None, SessionLoggerOptions::default()).info("noop", None);
        SessionLogger::create(Some(""), SessionLoggerOptions::default()).warn("noop", None);
    }

    #[test]
    fn only_allowlisted_non_blocked_keys_survive() {
        let line = format_line(
            "info",
            "turn",
            Some(&data(json!({ "attempt": 2, "password": "hunter2", "unknown": "x", "kind": "tool" }))),
        );
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(parsed["attempt"], json!(2));
        assert_eq!(parsed["kind"], json!("tool"));
        assert!(parsed.get("password").is_none());
        assert!(parsed.get("unknown").is_none());
    }

    #[test]
    fn sensitive_text_is_redacted_and_truncated() {
        assert_eq!(safe_text("Authorization: Bearer abc123"), "Authorization: Bearer [redacted]");
        let long = "x".repeat(500);
        let redacted = safe_text(&long);
        assert_eq!(redacted.chars().count(), MAX_STRING_LENGTH);
        assert!(redacted.ends_with("..."));
    }

    #[test]
    fn a_log_line_is_json_with_a_timestamp() {
        let line = format_line("debug", "boot", None);
        let parsed: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(parsed["level"], json!("debug"));
        assert_eq!(parsed["event"], json!("boot"));
        assert!(parsed["ts"].as_str().expect("ts").ends_with('Z'));
    }

    #[test]
    fn writing_appends_lines_and_rotates_at_the_cap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent = dir.path().to_string_lossy().into_owned();
        let logger = SessionLogger::create(
            Some(&agent),
            SessionLoggerOptions { max_bytes: Some(80), ..Default::default() },
        );
        for _ in 0..10 {
            logger.info("turn", None);
        }
        assert!(Path::new(&agent).join("logs/session.log").exists());
        assert!(Path::new(&agent).join("logs/session.log.1").exists());
    }

    #[test]
    fn a_sink_receives_every_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = Arc::clone(&seen);
        let logger = SessionLogger::create(
            Some(&dir.path().to_string_lossy()),
            SessionLoggerOptions {
                sink: Some(Arc::new(move |line: &str| sink_seen.lock().expect("lock").push(line.to_owned()))),
                ..Default::default()
            },
        );
        logger.warn("careful", None);
        assert_eq!(seen.lock().expect("lock").len(), 1);
    }
}
