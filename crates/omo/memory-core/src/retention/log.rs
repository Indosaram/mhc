//! Bounded append-only retention removal log (`runtime/retention-log.jsonl`) (#12932).

use std::collections::BTreeMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use serde_json::{Map, Value};

use crate::support::time::format_rfc3339_millis;

pub const RETENTION_LOG_FILE: &str = "retention-log.jsonl";
pub const RETENTION_LOG_PREVIOUS_FILE: &str = "retention-log.1.jsonl";
const DEFAULT_MAX_BYTES: u64 = 512 * 1024;
const DEFAULT_MAX_NAMES_PER_STORE: usize = 200;
/// Half of the 64 KiB reader tail so the newest whole line always fits inside it.
const MAX_LINE_BYTES: usize = 32 * 1024;
const TAIL_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone)]
pub struct RetentionLogInput {
    pub at: i64,
    pub removed: BTreeMap<String, Vec<String>>,
    pub cap_evicted_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionLogEntry {
    pub at: String,
    pub total: usize,
}

fn format_line(
    input: &RetentionLogInput,
    total: usize,
    stores: &[(&String, &Vec<String>)],
    max_names: usize,
) -> String {
    let mut removed = Map::new();
    let mut more = Map::new();
    for (store, names) in stores {
        let kept: Vec<Value> = names
            .iter()
            .take(max_names)
            .cloned()
            .map(Value::String)
            .collect();
        removed.insert((*store).clone(), Value::Array(kept));
        if names.len() > max_names {
            more.insert(
                (*store).clone(),
                Value::Number(((names.len() - max_names) as u64).into()),
            );
        }
    }
    let mut obj = Map::new();
    obj.insert("v".to_owned(), Value::Number(1u64.into()));
    obj.insert(
        "at".to_owned(),
        Value::String(format_rfc3339_millis(input.at)),
    );
    obj.insert("total".to_owned(), Value::Number((total as u64).into()));
    obj.insert("removed".to_owned(), Value::Object(removed));
    if !more.is_empty() {
        obj.insert("more".to_owned(), Value::Object(more));
    }
    if let Some(evicted) = input.cap_evicted_bytes
        && evicted > 0
    {
        obj.insert("capEvictedBytes".to_owned(), Value::Number(evicted.into()));
    }
    let json = serde_json::to_string(&Value::Object(obj)).unwrap_or_default();
    format!("{json}\n")
}

/// Appends one bounded JSON line (<= 32 KiB) to `retention-log.jsonl`, rotating at `max_bytes`.
/// No-op when `total == 0` so empty passes leave file mtimes untouched.
pub fn append_retention_log(
    runtime_dir: &Path,
    input: &RetentionLogInput,
    max_bytes: Option<u64>,
    max_names_per_store: Option<usize>,
) -> std::io::Result<()> {
    let stores: Vec<(&String, &Vec<String>)> = input
        .removed
        .iter()
        .filter(|(_, names)| !names.is_empty())
        .collect();
    let total: usize = stores.iter().map(|(_, names)| names.len()).sum();
    if total == 0 {
        return Ok(());
    }
    let mut max_names = max_names_per_store.unwrap_or(DEFAULT_MAX_NAMES_PER_STORE);
    let mut line;
    loop {
        line = format_line(input, total, &stores, max_names);
        if line.len() <= MAX_LINE_BYTES || max_names == 0 {
            break;
        }
        max_names /= 2;
    }

    let path = runtime_dir.join(RETENTION_LOG_FILE);
    let existing_size = match std::fs::metadata(&path) {
        Ok(meta) => meta.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
        Err(err) => return Err(err),
    };
    let cap = max_bytes.unwrap_or(DEFAULT_MAX_BYTES);
    if existing_size > 0 && existing_size.saturating_add(line.len() as u64) > cap {
        std::fs::rename(&path, runtime_dir.join(RETENTION_LOG_PREVIOUS_FILE))?;
    }

    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(&path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// Reads the newest whole entry in `retention-log.jsonl`, falling back to `retention-log.1.jsonl`.
pub fn read_last_retention_log_entry(
    runtime_dir: &Path,
) -> std::io::Result<Option<RetentionLogEntry>> {
    for name in [RETENTION_LOG_FILE, RETENTION_LOG_PREVIOUS_FILE] {
        let tail = read_tail(&runtime_dir.join(name))?;
        if let Some(entry) = last_entry(&tail) {
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

fn read_tail(path: &Path) -> std::io::Result<String> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(String::new());
        }
        Err(err) => return Err(err),
    };
    let size = file.metadata()?.len();
    let length = size.min(TAIL_BYTES) as usize;
    file.seek(SeekFrom::Start(size - length as u64))?;
    let mut buf = vec![0u8; length];
    file.read_exact(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn last_entry(tail: &str) -> Option<RetentionLogEntry> {
    for line in tail.split('\n').rev() {
        if line.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(at) = val.get("at").and_then(Value::as_str) else {
            continue;
        };
        let Some(total) = val.get("total").and_then(Value::as_u64) else {
            continue;
        };
        return Some(RetentionLogEntry {
            at: at.to_owned(),
            total: total as usize,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_single_line_to_32kib_and_rotates_past_max_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let names: Vec<String> = (0..1_000)
            .map(|i| format!("very-long-run-identifier-number-{i:04}-payload"))
            .collect();
        let input = RetentionLogInput {
            at: 1_700_000_000_000,
            removed: BTreeMap::from([("runs".to_owned(), names)]),
            cap_evicted_bytes: Some(4096),
        };
        append_retention_log(dir.path(), &input, Some(2048), Some(1_000)).unwrap();
        let raw = std::fs::read_to_string(dir.path().join(RETENTION_LOG_FILE)).unwrap();
        assert!(raw.len() <= MAX_LINE_BYTES);
        let parsed: Value = serde_json::from_str(raw.trim()).unwrap();
        assert_eq!(parsed["total"], 1_000);
        assert!(parsed["more"]["runs"].as_u64().unwrap() > 0);

        append_retention_log(dir.path(), &input, Some(2048), Some(1_000)).unwrap();
        assert!(dir.path().join(RETENTION_LOG_PREVIOUS_FILE).exists());
        let last = read_last_retention_log_entry(dir.path()).unwrap().unwrap();
        assert_eq!(last.total, 1_000);
    }
}
