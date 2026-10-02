use std::{fs::{self, OpenOptions}, io::Write, os::unix::fs::{DirBuilderExt, OpenOptionsExt}, path::{Path, PathBuf}};
use serde_json::{Map, Value, json};

const ALLOWED_KEYS: &[&str] = &["origin", "reason", "route", "variant", "generation", "requestId", "tokens", "tokensBefore", "savedTokens", "savingsRatio", "contextWindow", "threshold", "remainingSec", "count", "durationMs"];
const EVENTS: &[&str] = &["speculative_started", "speculative_applied", "speculative_stale", "speculative_invalidated", "idle_trigger", "idle_applied", "blocking_started", "warm_consumed", "core_route_generated", "skip_cap", "skip_breaker", "skip_cursor_mid_turn", "threshold_trigger", "hard_limit_trigger", "grace_deferred", "breaker_deterministic_fallback", "emergency_prune", "ineffective_counted", "summary_failed", "remote_aborted", "blocking_aborted"];

fn safe_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(object.iter().filter(|(key, _)| ALLOWED_KEYS.contains(&key.as_str())).map(|(key, value)| (key.clone(), safe_value(value))).collect()),
        Value::Array(values) => Value::Array(values.iter().map(safe_value).collect()),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => value.clone(),
    }
}

pub fn format_line(timestamp: &str, level: &str, event: &str, data: Option<&Map<String, Value>>) -> String {
    let mut entry = json!({"ts":timestamp,"level":level,"event":event});
    if let Some(data) = data {
        for (key, value) in data {
            if ALLOWED_KEYS.contains(&key.as_str()) { entry[key] = safe_value(value); }
        }
    }
    entry.to_string()
}

pub struct CompactionLogger {
    file_path: Option<PathBuf>,
    max_bytes: u64,
    mirror_to_stderr: bool,
    reported_failure: bool,
}

impl CompactionLogger {
    pub fn new(agent_dir: Option<&Path>, max_bytes: Option<u64>, mirror_to_stderr: Option<bool>) -> Self {
        let environment = std::env::vars().collect();
        Self {
            file_path: agent_dir.filter(|path| !path.as_os_str().is_empty()).map(|path| path.join("logs/compaction.log")),
            max_bytes: max_bytes.filter(|bytes| *bytes > 0).unwrap_or(5 * 1024 * 1024),
            mirror_to_stderr: mirror_to_stderr.unwrap_or_else(|| maho_core::brand::env_value("COMPACTION_DEBUG", &environment).as_deref() == Some("1")),
            reported_failure: false,
        }
    }

    pub fn log(&mut self, level: &str, event: &str, data: Option<&Map<String, Value>>, sink: Option<&dyn Fn(&str)>) {
        let Some(path) = &self.file_path else { return; };
        if !EVENTS.contains(&event) { return; }
        let line = format_line(&chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true), level, event, data);
        if let Some(sink) = sink { sink(&line); }
        let result = (|| -> std::io::Result<()> {
            if let Some(parent) = path.parent() { fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?; }
            let incoming = u64::try_from(line.len()).unwrap_or(u64::MAX).saturating_add(1);
            if fs::metadata(path).is_ok_and(|metadata| metadata.len().saturating_add(incoming) > self.max_bytes) {
                fs::rename(path, path.with_extension("log.1"))?;
            }
            let mut file = OpenOptions::new().append(true).create(true).mode(0o600).open(path)?;
            writeln!(file, "{line}")
        })();
        match result {
            Ok(()) => if self.mirror_to_stderr { eprintln!("[senpi-compaction] {line}"); },
            Err(error) => if !self.reported_failure { self.reported_failure = true; eprintln!("Unable to write compaction log {error}"); },
        }
    }

    pub fn debug(&mut self, event: &str, data: Option<&Map<String, Value>>) { self.log("debug", event, data, None); }
    pub fn info(&mut self, event: &str, data: Option<&Map<String, Value>>) { self.log("info", event, data, None); }
}
