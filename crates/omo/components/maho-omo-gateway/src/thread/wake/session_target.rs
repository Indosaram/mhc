//! Wake target resolution & session classification (`thread/wake/session-target.ts`).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::{Map, Value};

use super::super::address_book::{FindDiskSessionsOptions, find_disk_sessions_with_options};
use super::super::graceful_close_returns::SESSION_CLOSED_ENTRY;
use super::super::session_facts::{
    SESSION_FACTS_WINDOW_BYTES, parse_session_lines, summarize_session_entries,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeSessionKind {
    Interactive,
    Child,
    Worker,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeTarget {
    Gone,
    Ambiguous {
        session_paths: Vec<String>,
    },
    Found {
        session_path: String,
        cwd: String,
        session_kind: WakeSessionKind,
        closed_at: Option<i64>,
    },
}

const RPC_EFFECT_ENTRY: &str = "rpc_effect";

fn header_of(path: &Path) -> Option<Map<String, Value>> {
    let mut file = File::open(path).ok()?;
    let mut buf = vec![0u8; SESSION_FACTS_WINDOW_BYTES];
    let n = file.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let entries = parse_session_lines(&text, false, true);
    summarize_session_entries(&entries).header
}

fn close_instant_of(custom_type: &str, data: &Map<String, Value>) -> Option<i64> {
    let stamp = if custom_type == RPC_EFFECT_ENTRY {
        if data.get("kind").and_then(Value::as_str) == Some("unload")
            && data.get("state").and_then(Value::as_str) == Some("done")
        {
            data.get("updatedAt").and_then(Value::as_str)
        } else {
            None
        }
    } else if custom_type == SESSION_CLOSED_ENTRY {
        data.get("at").and_then(Value::as_str)
    } else {
        None
    }?;
    chrono::DateTime::parse_from_rfc3339(stamp)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn closed_at_of(path: &Path) -> Option<i64> {
    let mut file = File::open(path).ok()?;
    let size = file.metadata().ok()?.len() as usize;
    let start = size.saturating_sub(SESSION_FACTS_WINDOW_BYTES);
    file.seek(SeekFrom::Start(start as u64)).ok()?;
    let mut buf = vec![0u8; size - start];
    let n = file.read(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]);
    let mut closed_at: Option<i64> = None;
    for entry in parse_session_lines(&text, start > 0, false) {
        if entry.get("type").and_then(Value::as_str) != Some("custom") {
            continue;
        }
        let Some(data) = entry.get("data").and_then(Value::as_object) else {
            continue;
        };
        let Some(ct) = entry.get("customType").and_then(Value::as_str) else {
            continue;
        };
        if let Some(at) = close_instant_of(ct, data)
            && closed_at.is_none_or(|prev| at > prev)
        {
            closed_at = Some(at);
        }
    }
    closed_at
}

fn session_kind_of(header: Option<&Map<String, Value>>) -> WakeSessionKind {
    let Some(h) = header else {
        return WakeSessionKind::Unknown;
    };
    if h.get("kind").and_then(Value::as_str) == Some("worker") {
        return WakeSessionKind::Worker;
    }
    if h.get("spawnedBy").is_some_and(Value::is_object) {
        return WakeSessionKind::Child;
    }
    if h.get("kind").is_none() {
        return WakeSessionKind::Interactive;
    }
    WakeSessionKind::Unknown
}

pub fn resolve_wake_target(sessions_directory: &Path, durable_id: &str) -> WakeTarget {
    let found = find_disk_sessions_with_options(
        sessions_directory,
        durable_id,
        &FindDiskSessionsOptions {
            all_scope: false,
            workspace_roots: Vec::new(),
            id_only: true,
        },
    );
    if found.is_empty() {
        return WakeTarget::Gone;
    }
    if found.len() > 1 {
        return WakeTarget::Ambiguous {
            session_paths: found.into_iter().map(|s| s.session_path).collect(),
        };
    }
    let only = &found[0];
    let path = Path::new(&only.session_path);
    let closed_at = closed_at_of(path);
    WakeTarget::Found {
        session_path: only.session_path.clone(),
        cwd: only.cwd.clone(),
        session_kind: session_kind_of(header_of(path).as_ref()),
        closed_at,
    }
}
