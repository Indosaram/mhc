//! Usage records: extraction from an assistant message (port of oh-my-pi
//! `packages/stats/src/parser.ts` `extractStats`/`resolveUsageTotal`), the local ledger (clean-room,
//! docs/cleanroom/specs/telemetry.md "Ledger record") and the session-log scan (port of the
//! parser's file walk, `classifyAgentType` and `extractFolderFromPath`).
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// USD cost of one call as recorded on the message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub total: f64,
}

/// One completed model call.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    #[serde(default = "version")]
    pub v: u32,
    #[serde(default)]
    pub timestamp: i64,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub stop_reason: String,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    #[serde(default)]
    pub reasoning: u64,
    #[serde(default)]
    pub total_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<i64>,
}

fn version() -> u32 {
    1
}

pub const AGENT_MAIN: &str = "main";
pub const AGENT_SUBAGENT: &str = "subagent";

/// A persisted counter: finite non-negative numbers count (fractions truncated), anything else
/// (strings, negatives, absent) counts as 0 (oh-my-pi `finiteTokenCount`).
fn count(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(number)) => number.as_u64().unwrap_or_else(|| {
            number
                .as_f64()
                .filter(|value| value.is_finite() && *value >= 0.0)
                // Truncation of a finite, non-negative token count is the intent here.
                .map_or(0, |value| value.min(u64::MAX as f64) as u64)
        }),
        _ => 0,
    }
}

fn finite(value: Option<&Value>) -> Option<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
}

fn integer(value: Option<&Value>) -> Option<i64> {
    value
        .and_then(|value| value.as_i64().or_else(|| value.as_f64().map(|v| v as i64)))
        .filter(|value| *value != 0)
}

impl UsageRecord {
    /// Builds a record from a serialized assistant message (camelCase fields). Returns `None` when
    /// `model`/`provider` are not strings or `usage` is not an object (oh-my-pi skips those).
    pub fn from_assistant(message: &Value) -> Option<Self> {
        let model = message.get("model")?.as_str()?.to_owned();
        let provider = message.get("provider")?.as_str()?.to_owned();
        let api = message
            .get("api")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let usage = message.get("usage")?.as_object()?;
        let input = count(usage.get("input"));
        let output = count(usage.get("output"));
        let cache_read = count(usage.get("cacheRead"));
        let cache_write = count(usage.get("cacheWrite"));
        let total_tokens = match usage.get("totalTokens") {
            Some(Value::Number(number)) if number.as_f64().is_some_and(f64::is_finite) => {
                count(usage.get("totalTokens"))
            }
            _ => input + output + cache_read + cache_write,
        };
        let cost = usage
            .get("cost")
            .and_then(Value::as_object)
            .map(|cost| Cost {
                input: finite(cost.get("input")).unwrap_or(0.0),
                output: finite(cost.get("output")).unwrap_or(0.0),
                cache_read: finite(cost.get("cacheRead")).unwrap_or(0.0),
                cache_write: finite(cost.get("cacheWrite")).unwrap_or(0.0),
                total: finite(cost.get("total")).unwrap_or(0.0),
            });
        let stop_reason = match message.get("stopReason").and_then(Value::as_str) {
            Some(reason) => reason.to_owned(),
            None if message
                .get("errorMessage")
                .and_then(Value::as_str)
                .is_some() =>
            {
                "error".to_owned()
            }
            None => "aborted".to_owned(),
        };
        let timing = message.get("timing");
        let requested = timing.and_then(|t| integer(t.get("requestedAt")));
        let since_request = |key: &str| {
            let at = timing.and_then(|t| integer(t.get(key)))?;
            let delta = at - requested?;
            (delta >= 0).then_some(delta)
        };
        Some(Self {
            v: 1,
            timestamp: integer(message.get("timestamp"))
                .filter(|t| *t > 0)
                .unwrap_or(0),
            provider,
            model,
            api,
            stop_reason,
            input,
            output,
            cache_read,
            cache_write,
            reasoning: count(usage.get("reasoning")),
            total_tokens,
            cost,
            duration_ms: since_request("endedAt"),
            ttft_ms: since_request("firstTokenAt"),
            ..Self::default()
        })
    }

    /// `true` when the call was a failure (`stopReason == "error"`).
    pub fn failed(&self) -> bool {
        self.stop_reason == "error"
    }
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// `main`, or `subagent` when `depth` (`$MHC_TASK_DEPTH`) is a positive integer.
pub fn agent_kind(depth: Option<&str>) -> &'static str {
    match depth.and_then(|depth| depth.trim().parse::<u64>().ok()) {
        Some(depth) if depth > 0 => AGENT_SUBAGENT,
        _ => AGENT_MAIN,
    }
}

/// The append-only local ledger.
#[derive(Clone, Debug)]
pub struct Ledger {
    pub path: PathBuf,
}

impl Ledger {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Appends one record as one line (a single `write`), creating the directory on first use.
    pub fn append(&self, record: &UsageRecord) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }

    /// Every parseable record; a missing file is an empty ledger.
    pub fn read(&self) -> std::io::Result<Vec<UsageRecord>> {
        let file = match std::fs::File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        Ok(BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<UsageRecord>(line.trim()).ok())
            .collect())
    }
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_jsonl(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "jsonl") {
            out.push(path);
        }
    }
}

/// `--work-pi--` → `work-pi` (the session directory name with its `--` fences removed).
fn folder_from_dir(sessions_dir: &Path, file: &Path) -> String {
    let relative = file.strip_prefix(sessions_dir).unwrap_or(file);
    let first = relative
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .unwrap_or_default();
    let trimmed = first.strip_prefix("--").unwrap_or(&first);
    let trimmed = trimmed.strip_suffix("--").unwrap_or(trimmed);
    format!("/{trimmed}")
}

fn iso_ms(value: Option<&Value>) -> i64 {
    value
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<jiff::Timestamp>().ok())
        .map_or(0, |timestamp| timestamp.as_millisecond())
}

/// Records from one session file.
pub fn scan_session_file(sessions_dir: &Path, file: &Path) -> Vec<UsageRecord> {
    let Ok(handle) = std::fs::File::open(file) else {
        return Vec::new();
    };
    let depth = file
        .strip_prefix(sessions_dir)
        .map_or(0, |relative| relative.components().count());
    let agent = if depth > 2 {
        AGENT_SUBAGENT
    } else {
        AGENT_MAIN
    };
    let mut cwd = folder_from_dir(sessions_dir, file);
    let mut session_id = String::new();
    let mut records = Vec::new();
    for line in BufReader::new(handle).lines().map_while(Result::ok) {
        let Ok(entry) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        match entry.get("type").and_then(Value::as_str) {
            Some("session") => {
                if let Some(dir) = entry.get("cwd").and_then(Value::as_str) {
                    cwd = dir.to_owned();
                }
                if let Some(id) = entry.get("id").and_then(Value::as_str) {
                    id.clone_into(&mut session_id);
                }
            }
            Some("message") => {
                if entry
                    .get("id")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    continue;
                }
                let Some(message) = entry.get("message") else {
                    continue;
                };
                if message.get("role").and_then(Value::as_str) != Some("assistant")
                    || message.get("api").and_then(Value::as_str).is_none()
                {
                    continue;
                }
                let Some(mut record) = UsageRecord::from_assistant(message) else {
                    continue;
                };
                if record.timestamp == 0 {
                    record.timestamp = iso_ms(entry.get("timestamp"));
                }
                record.cwd.clone_from(&cwd);
                record.session_id.clone_from(&session_id);
                record.agent = agent.to_owned();
                records.push(record);
            }
            _ => {}
        }
    }
    records
}

/// Records from every `*.jsonl` under `sessions_dir`, recursively, in path order.
pub fn scan_sessions(sessions_dir: &Path) -> Vec<UsageRecord> {
    let mut files = Vec::new();
    collect_jsonl(sessions_dir, &mut files);
    files
        .iter()
        .flat_map(|file| scan_session_file(sessions_dir, file))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assistant() -> Value {
        json!({
            "role": "assistant", "api": "openai-completions", "provider": "offline", "model": "m1",
            "usage": {"input": 100, "output": 20, "cacheRead": 300, "cacheWrite": 0, "totalTokens": 420,
                      "cost": {"input": 0.001, "output": 0.002, "cacheRead": 0.0003, "cacheWrite": 0.0, "total": 0.0033}},
            "stopReason": "stop", "timestamp": 1_700_000_000_000_i64,
            "timing": {"requestedAt": 1_700_000_000_000_i64, "firstTokenAt": 1_700_000_000_250_i64, "endedAt": 1_700_000_001_000_i64},
            "content": [{"type": "text", "text": "secret answer"}]
        })
    }

    #[test]
    fn extracts_counters_cost_and_timing_but_no_content() {
        let record = UsageRecord::from_assistant(&assistant()).expect("record");
        assert_eq!(
            (
                record.input,
                record.output,
                record.cache_read,
                record.total_tokens
            ),
            (100, 20, 300, 420)
        );
        assert_eq!(record.duration_ms, Some(1000));
        assert_eq!(record.ttft_ms, Some(250));
        assert_eq!(record.cost.expect("cost").total, 0.0033);
        let line = serde_json::to_string(&record).expect("json");
        assert!(!line.contains("secret"));
    }

    #[test]
    fn malformed_counters_count_as_zero_and_totals_are_derived() {
        let message = json!({
            "provider": "p", "model": "m", "errorMessage": "boom",
            "usage": {"input": "10", "output": 5, "cacheRead": -3, "cacheWrite": 2.7, "totalTokens": "x"}
        });
        let record = UsageRecord::from_assistant(&message).expect("record");
        assert_eq!(
            (
                record.input,
                record.output,
                record.cache_read,
                record.cache_write
            ),
            (0, 5, 0, 2)
        );
        assert_eq!(record.total_tokens, 7);
        assert_eq!(record.stop_reason, "error");
        assert!(record.failed());
        assert!(record.cost.is_none());
        assert!(UsageRecord::from_assistant(&json!({"provider": "p", "model": "m"})).is_none());
        let aborted =
            UsageRecord::from_assistant(&json!({"provider": "p", "model": "m", "usage": {}}));
        assert_eq!(aborted.expect("record").stop_reason, "aborted");
    }

    #[test]
    fn ledger_appends_lines_and_skips_garbage() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = Ledger::new(dir.path().join("nested/usage.jsonl"));
        assert!(ledger.read().expect("read").is_empty());
        let mut record = UsageRecord::from_assistant(&assistant()).expect("record");
        record.session_id = "s1".into();
        ledger.append(&record).expect("append");
        ledger.append(&record).expect("append");
        let mut text = std::fs::read_to_string(&ledger.path).expect("file");
        assert_eq!(text.lines().count(), 2);
        text.push_str("not json\n");
        std::fs::write(&ledger.path, text).expect("write");
        let read = ledger.read().expect("read");
        assert_eq!(read, vec![record.clone(), record]);
    }

    #[test]
    fn agent_kind_reads_the_task_depth() {
        assert_eq!(agent_kind(None), AGENT_MAIN);
        assert_eq!(agent_kind(Some("0")), AGENT_MAIN);
        assert_eq!(agent_kind(Some("2")), AGENT_SUBAGENT);
        assert_eq!(agent_kind(Some("x")), AGENT_MAIN);
    }

    #[test]
    fn session_scan_reads_headers_assistant_entries_and_nesting() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = dir.path();
        let project = sessions.join("--work-app--");
        std::fs::create_dir_all(project.join("s1")).expect("dirs");
        let mut message = assistant();
        message["timestamp"] = json!(0);
        let lines = [
            json!({"type": "session", "id": "s1", "cwd": "/work/app"}).to_string(),
            json!({"type": "message", "id": "a", "timestamp": "2026-10-27T00:00:00Z", "message": message}).to_string(),
            json!({"type": "message", "id": "", "message": assistant()}).to_string(),
            json!({"type": "message", "id": "u", "message": {"role": "user", "content": "hi"}}).to_string(),
            "{broken".to_owned(),
        ];
        std::fs::write(project.join("main.jsonl"), lines.join("\n")).expect("write");
        let child = [json!({"type": "message", "id": "c", "message": assistant()}).to_string()];
        std::fs::write(project.join("s1/child.jsonl"), child.join("\n")).expect("write");
        let records = scan_sessions(sessions);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].cwd, "/work/app");
        assert_eq!(records[0].session_id, "s1");
        assert_eq!(records[0].agent, AGENT_MAIN);
        assert_eq!(records[0].timestamp, 1_793_059_200_000);
        assert_eq!(records[1].agent, AGENT_SUBAGENT);
        assert_eq!(records[1].cwd, "/work-app");
    }
}
