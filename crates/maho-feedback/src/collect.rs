//! Host-collected report sections (spec 4.2): environment facts, the structural config pass,
//! recent log events and the session transcript. Every function here is pure over its inputs so
//! tests can drive it with fixed clocks, environments and directories.

use jiff::{SignedDuration, Timestamp, tz::TimeZone};
use maho_ext_api::SessionEntry;
use regex::Regex;
use serde_json::{Map, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::OnceLock;

/// Replacement for a scrubbed config value.
pub const REDACTED: &str = "<redacted>";
/// Bytes read from the end of each log file.
pub const LOG_TAIL_BYTES: u64 = 512 * 1024;
/// Log lines older than this are left out.
pub const LOG_WINDOW: SignedDuration = SignedDuration::from_secs(15 * 60);
/// Projected log lines kept per file.
pub const LOG_LINES_PER_FILE: usize = 200;
/// Longest kept string value in a projected log line, in UTF-16 units.
pub const LOG_STRING_MAX_UTF16: usize = 160;
/// Messages kept in the transcript.
pub const TRANSCRIPT_MESSAGES: usize = 50;
/// Bodies longer than this many characters are shortened.
pub const BODY_MAX_CHARS: usize = 2400;
/// Characters kept at each end of a shortened body.
pub const BODY_KEEP_CHARS: usize = 1200;

// ---------------------------------------------------------------------------------------------
// Environment

/// The locale: the first of `LC_ALL`, `LC_MESSAGES`, `LANG` whose part before `.` is non-empty
/// and not `C`/`POSIX`, with `_` turned into `-`; else `unknown`.
pub fn locale(env: &dyn Fn(&str) -> Option<String>) -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(|key| env(key))
        .find_map(|value| {
            let base = value.split('.').next().unwrap_or_default().trim();
            (!base.is_empty() && base != "C" && base != "POSIX").then(|| base.replace('_', "-"))
        })
        .unwrap_or_else(|| "unknown".into())
}

/// `beta` for a pre-release product version (contains `-`), else `latest`.
pub fn channel(product_version: &str) -> &'static str {
    if product_version.contains('-') {
        "beta"
    } else {
        "latest"
    }
}

/// RFC 3339 UTC with milliseconds, e.g. `2025-06-07T22:15:09.123Z`.
pub fn rfc3339_millis(at: Timestamp) -> String {
    let dt = TimeZone::UTC.to_datetime(at);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        dt.year(),
        dt.month(),
        dt.day(),
        dt.hour(),
        dt.minute(),
        dt.second(),
        dt.millisecond()
    )
}

/// `+HH:MM` / `-HH:MM` for an offset in seconds.
pub fn format_offset(seconds: i32) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let minutes = seconds.unsigned_abs() / 60;
    format!("{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// `TZ` when set and non-empty, else the local UTC offset at `at`.
pub fn time_zone(env: &dyn Fn(&str) -> Option<String>, at: Timestamp) -> String {
    env("TZ")
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format_offset(TimeZone::system().to_offset(at).seconds()))
}

/// The OS kernel release, or `unknown` where the OS does not expose it.
pub fn kernel_release() -> String {
    read_kernel_release()
        .map(|release| release.trim().to_owned())
        .filter(|release| !release.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

#[cfg(target_os = "linux")]
fn read_kernel_release() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()
}

#[cfg(all(unix, not(target_os = "linux")))]
fn read_kernel_release() -> Option<String> {
    let output = std::process::Command::new("uname")
        .arg("-r")
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(not(unix))]
fn read_kernel_release() -> Option<String> {
    None
}

/// Facts of the `environment` section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentFacts {
    pub product_version: String,
    pub host_version: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub locale: String,
    pub channel: String,
    pub report_time: String,
    pub time_zone: String,
}

impl EnvironmentFacts {
    /// Label-aligned lines.
    pub fn render(&self) -> String {
        aligned(&[
            ("Product version", self.product_version.as_str()),
            ("Host version", self.host_version.as_str()),
            ("OS", self.os.as_str()),
            ("Kernel release", self.kernel.as_str()),
            ("Architecture", self.arch.as_str()),
            ("Locale", self.locale.as_str()),
            ("Release channel", self.channel.as_str()),
            ("Report time", self.report_time.as_str()),
            ("Time zone", self.time_zone.as_str()),
        ])
    }
}

/// `label: value` lines with the values starting in one column.
pub fn aligned(lines: &[(&str, &str)]) -> String {
    let width = lines
        .iter()
        .map(|(label, _)| label.chars().count() + 1)
        .max()
        .unwrap_or(0);
    lines
        .iter()
        .map(|(label, value)| format!("{:<width$} {value}", format!("{label}:")))
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------------------------
// Config pass

/// Final key words that mark a credential.
const SECRET_WORDS: &[&str] = &[
    "key",
    "keys",
    "apikey",
    "apikeys",
    "token",
    "tokens",
    "secret",
    "secrets",
    "password",
    "passwords",
    "passwd",
    "passphrase",
    "auth",
    "authorization",
    "cookie",
    "cookies",
    "credential",
    "credentials",
    "bearer",
];

/// Words naming a numeric bound or size; a key containing one is never secret.
const BOUND_WORDS: &[&str] = &[
    "max",
    "min",
    "limit",
    "limits",
    "size",
    "window",
    "count",
    "length",
    "len",
    "budget",
    "threshold",
    "cap",
    "quota",
    "ttl",
    "timeout",
];

/// Snake-case words of a key: camelCase humps and `-` become `_`, lower-cased.
pub fn key_words(key: &str) -> Vec<String> {
    let mut snake = String::with_capacity(key.len() + 4);
    let mut previous: Option<char> = None;
    for c in key.chars() {
        if c.is_uppercase() && previous.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()) {
            snake.push('_');
        }
        snake.push(if c == '-' || c == ' ' || c == '.' {
            '_'
        } else {
            c
        });
        previous = Some(c);
    }
    snake
        .to_lowercase()
        .split('_')
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether a value under `key` is a credential.
pub fn is_secret_key(key: &str) -> bool {
    let words = key_words(key);
    let Some(last) = words.last() else {
        return false;
    };
    SECRET_WORDS.contains(&last.as_str())
        && !words
            .iter()
            .any(|word| BOUND_WORDS.contains(&word.as_str()))
}

fn is_blanked_container_key(key: &str) -> bool {
    key.eq_ignore_ascii_case("headers") || key.eq_ignore_ascii_case("env")
}

fn userinfo_pattern() -> Option<&'static Regex> {
    static PATTERN: OnceLock<Option<Regex>> = OnceLock::new();
    PATTERN
        .get_or_init(|| Regex::new(r"([A-Za-z][A-Za-z0-9+.\-]*://)[^/\s@?#]+@").ok())
        .as_ref()
}

/// `scheme://user:pass@host` becomes `scheme://<redacted>@host` anywhere in `text`.
pub fn strip_userinfo(text: &str) -> String {
    match userinfo_pattern() {
        Some(pattern) => pattern
            .replace_all(text, format!("${{1}}{REDACTED}@").as_str())
            .into_owned(),
        None => text.to_owned(),
    }
}

/// Every leaf replaced by [`REDACTED`], structure kept.
fn blank_leaves(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, child)| (key.clone(), blank_leaves(child)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(blank_leaves).collect()),
        _ => Value::String(REDACTED.into()),
    }
}

/// The structural secret pass over a configuration value (spec 4.2 `config`).
pub fn scrub_config(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, child) in map {
                let scrubbed =
                    if is_secret_key(key) || (is_blanked_container_key(key) && child.is_object()) {
                        blank_leaves(child)
                    } else {
                        scrub_config(child)
                    };
                out.insert(key.clone(), scrubbed);
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(scrub_config).collect()),
        Value::String(text) => Value::String(strip_userinfo(text)),
        other => other.clone(),
    }
}

/// The config section text before the upload redactor: scrubbed, pretty-printed JSON.
pub fn config_text(view: &Value) -> String {
    serde_json::to_string_pretty(&scrub_config(view)).unwrap_or_else(|_| "{}".into())
}

// ---------------------------------------------------------------------------------------------
// Logs

/// Fields of a log line that never carry user content.
const LOG_FIELDS: &[&str] = &[
    "ts",
    "timestamp",
    "time",
    "level",
    "severity",
    "event",
    "component",
    "kind",
    "status",
    "statusCode",
    "status_code",
    "code",
    "errorCode",
    "error_code",
    "durationMs",
    "duration_ms",
    "elapsedMs",
    "elapsed_ms",
    "latencyMs",
    "latency_ms",
    "attempt",
    "attempts",
    "turn",
    "turnIndex",
    "turn_index",
    "model",
    "provider",
    "sessionId",
    "session_id",
    "taskId",
    "task_id",
    "requestId",
    "request_id",
    "pid",
    "exitCode",
    "exit_code",
    "signal",
];

fn line_time(object: &Map<String, Value>) -> Option<Timestamp> {
    ["ts", "timestamp"].into_iter().find_map(|field| {
        object
            .get(field)
            .and_then(Value::as_str)
            .and_then(|text| text.parse::<Timestamp>().ok())
    })
}

fn keeps_value(value: &Value) -> bool {
    match value {
        Value::Number(_) | Value::Bool(_) => true,
        Value::String(text) => {
            !text.contains(['\n', '\r']) && text.encode_utf16().count() <= LOG_STRING_MAX_UTF16
        }
        _ => false,
    }
}

/// One log line projected onto the allowlist, or `None` when it is not a recent JSON object.
pub fn project_log_line(line: &str, now: Timestamp) -> Option<String> {
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(line.trim()) else {
        return None;
    };
    let at = line_time(&object)?;
    if now.duration_since(at) > LOG_WINDOW {
        return None;
    }
    let projected: Map<String, Value> = object
        .into_iter()
        .filter(|(key, value)| LOG_FIELDS.contains(&key.as_str()) && keeps_value(value))
        .collect();
    serde_json::to_string(&Value::Object(projected)).ok()
}

/// The last [`LOG_TAIL_BYTES`] of `path`, without a partial first line.
fn read_tail(path: &Path) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(LOG_TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start == 0 {
        return Some(text);
    }
    Some(match text.split_once('\n') {
        Some((_, rest)) => rest.to_owned(),
        None => String::new(),
    })
}

/// The `logs` section text from `<agent dir>/logs/*.log`, or `None` when nothing qualifies.
pub fn collect_logs(logs_dir: &Path, now: Timestamp) -> Option<String> {
    let mut files: Vec<(String, std::path::PathBuf)> = std::fs::read_dir(logs_dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            name.ends_with(".log").then(|| (name, entry.path()))
        })
        .collect();
    files.sort();
    let blocks: Vec<String> = files
        .into_iter()
        .filter_map(|(name, path)| {
            let text = read_tail(&path)?;
            let lines: Vec<String> = text
                .lines()
                .filter_map(|line| project_log_line(line, now))
                .collect();
            if lines.is_empty() {
                return None;
            }
            let kept = &lines[lines.len().saturating_sub(LOG_LINES_PER_FILE)..];
            Some(format!("## {name}\n{}", kept.join("\n")))
        })
        .collect();
    (!blocks.is_empty()).then(|| blocks.join("\n\n"))
}

// ---------------------------------------------------------------------------------------------
// Transcript

/// Header facts of the `session` section; `None` values print as `unknown`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionHeader {
    pub session_id: Option<String>,
    /// `provider/id`.
    pub model: Option<String>,
    pub cwd: Option<String>,
}

/// A body over [`BODY_MAX_CHARS`] keeps its first and last [`BODY_KEEP_CHARS`] characters around
/// a marker naming how many were cut.
pub fn shorten_body(body: &str) -> String {
    let total = body.chars().count();
    if total <= BODY_MAX_CHARS {
        return body.to_owned();
    }
    let cut = total - 2 * BODY_KEEP_CHARS;
    let head: String = body.chars().take(BODY_KEEP_CHARS).collect();
    let tail: String = body.chars().skip(total - BODY_KEEP_CHARS).collect();
    format!("{head} [... {cut} characters cut ...] {tail}")
}

fn part_text(part: &Value) -> Option<String> {
    match part.get("type").and_then(Value::as_str)? {
        "text" => part.get("text").and_then(Value::as_str).map(str::to_owned),
        "toolCall" => {
            let name = part.get("name").and_then(Value::as_str).unwrap_or("tool");
            let arguments = part
                .get("arguments")
                .map(|args| serde_json::to_string(args).unwrap_or_default())
                .unwrap_or_else(|| "{}".into());
            Some(format!("[{name}] {arguments}"))
        }
        "image" => Some("[image]".into()),
        _ => None,
    }
}

/// The body of a message: text parts joined by spaces, tool calls, images.
pub fn message_body(message: &Value) -> String {
    match message.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(part_text)
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

fn entry_message(entry: &SessionEntry) -> Option<&Value> {
    if entry.kind != "message" {
        return None;
    }
    match entry.data.get("message") {
        Some(message) if message.is_object() => Some(message),
        _ => entry.data.get("role").is_some().then_some(&entry.data),
    }
}

fn clock_time(timestamp: &str) -> Option<String> {
    let at = timestamp.trim().parse::<Timestamp>().ok()?;
    let dt = TimeZone::UTC.to_datetime(at);
    Some(format!(
        "{:02}:{:02}:{:02}",
        dt.hour(),
        dt.minute(),
        dt.second()
    ))
}

/// One transcript line (plus indented continuation lines) for a `message` entry.
pub fn transcript_line(entry: &SessionEntry) -> Option<String> {
    let message = entry_message(entry)?;
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let body = shorten_body(&message_body(message));
    let mut lines = body.lines().filter(|line| !line.trim().is_empty());
    let first = lines.next().unwrap_or_default();
    let mut out = match clock_time(&entry.timestamp) {
        Some(time) => format!("{role} ({time}): {first}"),
        None => format!("{role}: {first}"),
    };
    for line in lines {
        out.push_str("\n  ");
        out.push_str(line);
    }
    Some(out.trim_end().to_owned())
}

/// The `session` section text: header, then the last [`TRANSCRIPT_MESSAGES`] messages.
pub fn render_session(header: &SessionHeader, entries: &[SessionEntry]) -> String {
    let unknown = |value: &Option<String>| value.clone().unwrap_or_else(|| "unknown".into());
    let mut out = aligned(&[
        ("Session", unknown(&header.session_id).as_str()),
        ("Model", unknown(&header.model).as_str()),
        ("Working directory", unknown(&header.cwd).as_str()),
    ]);
    let lines: Vec<String> = entries.iter().filter_map(transcript_line).collect();
    let kept = &lines[lines.len().saturating_sub(TRANSCRIPT_MESSAGES)..];
    if !kept.is_empty() {
        out.push_str("\n\n");
        out.push_str(&kept.join("\n"));
    }
    out
}
