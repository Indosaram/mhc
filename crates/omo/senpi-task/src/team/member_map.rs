//! Sidecar mapping of team member name -> the TaskManager `st_` id that backs it. Persisted next to
//! the team-core runtime state so `deleteTeam` (and a future reconcile) can find the member tasks
//! without holding process-only state.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

pub type MemberTaskMap = BTreeMap<String, String>;

const MEMBER_MAP_FILE: &str = "senpi-task-members.json";

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn member_task_map_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(MEMBER_MAP_FILE)
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{nanos:x}-{counter:x}")
}

/// Atomically writes the member -> task map: a uniquely-named temp sibling is written then renamed
/// over the target, so a crash mid-write never leaves a torn or partial map on disk.
pub fn write_member_task_map(runtime_dir: &Path, map: &MemberTaskMap) -> io::Result<()> {
    let target = member_task_map_path(runtime_dir);
    let mut temp_name = target.clone().into_os_string();
    temp_name.push(format!(".{}.{}.tmp", std::process::id(), unique_suffix()));
    let temp_path = PathBuf::from(temp_name);
    let serialized = serde_json::to_string_pretty(map).map_err(io::Error::other)?;
    std::fs::write(&temp_path, format!("{serialized}\n"))?;
    std::fs::rename(&temp_path, &target)
}

/// Reads the sidecar map. A missing file or malformed content yields an empty map so callers can
/// treat a fresh or corrupted runtime dir as "no members recorded" instead of failing.
pub fn read_member_task_map(runtime_dir: &Path) -> MemberTaskMap {
    let Ok(raw) = std::fs::read_to_string(member_task_map_path(runtime_dir)) else {
        return MemberTaskMap::new();
    };
    let Ok(parsed) = serde_json::from_str::<Value>(&raw) else {
        return MemberTaskMap::new();
    };
    to_string_record(&parsed).unwrap_or_default()
}

fn to_string_record(value: &Value) -> Option<MemberTaskMap> {
    let record = value.as_object()?;
    let mut map = MemberTaskMap::new();
    for (key, entry) in record {
        map.insert(key.clone(), entry.as_str()?.to_string());
    }
    Some(map)
}
