use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value, json};

use super::types::{PersistedTaskEvent, StoreError};
use crate::state::TaskId;

const REDACTED_VALUE: &str = "[REDACTED]";

static SENSITIVE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)token|password|secret|authorization|api[_-]?key")
        .unwrap_or_else(|error| panic!("static regex: {error}"))
});

pub fn redact_event_payload(value: &Value) -> Value {
    match value {
        Value::Array(entries) => Value::Array(entries.iter().map(redact_event_payload).collect()),
        Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, entry)| {
                    let redacted = if SENSITIVE_KEY.is_match(key) {
                        Value::String(REDACTED_VALUE.to_string())
                    } else {
                        redact_event_payload(entry)
                    };
                    (key.clone(), redacted)
                })
                .collect::<Map<String, Value>>(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
    }
}

pub(crate) fn event_log_path(state_dir: &Path, task_id: TaskId) -> PathBuf {
    state_dir.join("logs").join(format!("{task_id}.jsonl"))
}

pub(crate) fn append_task_event(
    state_dir: &Path,
    task_id: TaskId,
    event: &PersistedTaskEvent,
) -> Result<PathBuf, StoreError> {
    std::fs::create_dir_all(state_dir.join("logs"))?;
    let path = event_log_path(state_dir, task_id);
    let line = json!({ "type": event.event_type, "payload": redact_event_payload(&event.payload) });
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(file, "{line}")?;
    Ok(path)
}
