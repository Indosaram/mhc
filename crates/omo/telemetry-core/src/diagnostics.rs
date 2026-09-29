use std::fs;
use std::io;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::NaiveDate;
use chrono::TimeDelta;
use chrono::Utc;
use serde_json::Map;
use serde_json::Value;
use utils::atomic_write::write_file_atomically;

use crate::types::TelemetryDiagnosticErrorKind;
use crate::types::TelemetryDiagnosticInput;

const DIAGNOSTICS_FILE_NAME: &str = "telemetry-diagnostics.jsonl";
const DIAGNOSTICS_RETENTION_DAYS: i64 = 7;
const DIAGNOSTICS_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct WriteTelemetryDiagnosticOptions<'a> {
    pub diagnostics_dir: &'a Path,
    pub now: Option<DateTime<Utc>>,
}

pub fn get_telemetry_diagnostics_file_path(diagnostics_dir: &Path) -> PathBuf {
    diagnostics_dir.join(DIAGNOSTICS_FILE_NAME)
}

pub(crate) fn to_iso_string(date: DateTime<Utc>) -> String {
    date.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Appends one JSONL record; every I/O failure is swallowed, as in the TS original.
pub fn write_telemetry_diagnostic(
    input: &TelemetryDiagnosticInput,
    options: &WriteTelemetryDiagnosticOptions<'_>,
) {
    let now = options.now.unwrap_or_else(Utc::now);
    cleanup_telemetry_diagnostics(&WriteTelemetryDiagnosticOptions {
        diagnostics_dir: options.diagnostics_dir,
        now: Some(now),
    });
    let _ignored = append_record(options.diagnostics_dir, input, now);
}

fn append_record(
    diagnostics_dir: &Path,
    input: &TelemetryDiagnosticInput,
    now: DateTime<Utc>,
) -> io::Result<()> {
    fs::create_dir_all(diagnostics_dir)?;
    let line = serde_json::to_string(&to_diagnostic_record(input, now))?;
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(get_telemetry_diagnostics_file_path(diagnostics_dir))?
        .write_all(format!("{line}\n").as_bytes())
}

/// Drops records older than seven days and keeps at most the newest 256 KiB of lines.
pub fn cleanup_telemetry_diagnostics(options: &WriteTelemetryDiagnosticOptions<'_>) {
    let path = get_telemetry_diagnostics_file_path(options.diagnostics_dir);
    if !path.exists() {
        return;
    }
    let cutoff = options.now.unwrap_or_else(Utc::now) - TimeDelta::days(DIAGNOSTICS_RETENTION_DAYS);
    let Ok(content) = fs::read_to_string(&path) else {
        return;
    };
    let lines: Vec<&str> = content
        .split('\n')
        .filter(|line| should_retain_line(line, cutoff))
        .collect();
    let retained = trim_to_max_bytes(&lines);
    let next = if retained.is_empty() {
        String::new()
    } else {
        format!("{}\n", retained.join("\n"))
    };
    let _ignored = write_file_atomically(&path, &next);
}

fn to_diagnostic_record(
    input: &TelemetryDiagnosticInput,
    now: DateTime<Utc>,
) -> Map<String, Value> {
    let mut record = Map::new();
    record.insert("timestamp".into(), to_iso_string(now).into());
    record.insert("event".into(), input.event.as_str().into());
    record.insert("source".into(), input.source.clone().into());
    if let Some(error) = &input.error {
        let kind = input
            .error_kind
            .unwrap_or(TelemetryDiagnosticErrorKind::Error);
        record.insert("error_kind".into(), kind.as_str().into());
        record.insert("error_name".into(), error.name.clone().into());
        record.insert("error_message".into(), error.message.clone().into());
    }
    record
}

fn parse_timestamp(timestamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(timestamp)
        .map(|parsed| parsed.with_timezone(&Utc))
        .ok()
        .or_else(|| {
            NaiveDate::parse_from_str(timestamp, "%Y-%m-%d")
                .ok()
                .and_then(|date| date.and_hms_opt(0, 0, 0))
                .map(|naive| naive.and_utc())
        })
}

fn should_retain_line(line: &str, cutoff: DateTime<Utc>) -> bool {
    if line.is_empty() {
        return false;
    }
    let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(line) else {
        return false;
    };
    parsed
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(parse_timestamp)
        .is_some_and(|timestamp| timestamp >= cutoff)
}

fn trim_to_max_bytes<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    let mut retained = Vec::new();
    let mut total_bytes = 0;
    for line in lines.iter().rev() {
        let line_bytes = line.len() + 1;
        if total_bytes + line_bytes > DIAGNOSTICS_MAX_BYTES {
            break;
        }
        retained.push(*line);
        total_bytes += line_bytes;
    }
    retained.reverse();
    retained
}
