//! Durable reflection run-id high-water mark so reaped/removed traces never collide with fresh run ids.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::support::random::random_uuid;

pub const REFLECTION_RUN_ID_PREFIX: &str = "reflection-run-";
const RESERVATION_STATE_FILES: &[&str] = &["active.lock", "pending.json"];
const RUN_ID_MARK_FILE: &str = "run-id.json";

/// Subset of identity paths needed to scan live reflection traces on disk.
#[derive(Debug, Clone)]
pub struct RunIdHighWaterPaths {
    pub reflection: PathBuf,
    pub reflection_sessions: PathBuf,
    pub worktrees: PathBuf,
}

/// Parses `reflection-run-<n>`, `reflection-run-<n>.json`, or `<epoch>-reflection-run-<n>`.
pub fn reflection_run_number(name: &str) -> u64 {
    let base = name.rsplit('/').next().unwrap_or(name);
    let stem = base.strip_suffix(".json").unwrap_or(base);
    let tail = if let Some(rest) = stem.strip_prefix(REFLECTION_RUN_ID_PREFIX) {
        rest
    } else if let Some((epoch, rest)) = stem.split_once("-reflection-run-")
        && !epoch.is_empty()
        && epoch.chars().all(|c| c.is_ascii_digit())
    {
        rest
    } else {
        return 0;
    };
    if tail.is_empty() || !tail.chars().all(|c| c.is_ascii_digit()) {
        return 0;
    }
    tail.parse::<u64>().unwrap_or(0)
}

fn highest_named_run(directory: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .map(|name| reflection_run_number(&name))
        .max()
        .unwrap_or(0)
}

fn reserved_run(path: &Path) -> u64 {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return 0;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return 0;
    };
    value
        .get("runId")
        .and_then(serde_json::Value::as_str)
        .map_or(0, reflection_run_number)
}

/// Highest run number any trace on disk still carries (`completions`, `runs`, `reflection-sessions`,
/// `worktrees`, `active.lock`, `pending.json`).
pub fn scan_reflection_run_high_water(paths: &RunIdHighWaterPaths) -> u64 {
    [
        highest_named_run(&paths.reflection.join("completions")),
        highest_named_run(&paths.reflection.join("runs")),
        highest_named_run(&paths.reflection_sessions),
        highest_named_run(&paths.worktrees),
        reserved_run(&paths.reflection.join(RESERVATION_STATE_FILES[0])),
        reserved_run(&paths.reflection.join(RESERVATION_STATE_FILES[1])),
    ]
    .into_iter()
    .max()
    .unwrap_or(0)
}

fn reflection_run_id_mark_path(reflection_dir: &Path) -> PathBuf {
    reflection_dir.join(RUN_ID_MARK_FILE)
}

/// The durable high-water mark, or `None` when absent or unreadable.
pub fn read_reflection_run_id_mark(reflection_dir: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(reflection_run_id_mark_path(reflection_dir)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value.get("highWater").and_then(serde_json::Value::as_u64)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RunIdMark {
    version: u32,
    high_water: u64,
}

/// Raises the durable mark to at least `high_water` and never lowers it.
pub fn record_reflection_run_id_mark(
    reflection_dir: &Path,
    high_water: u64,
) -> std::io::Result<u64> {
    let current = read_reflection_run_id_mark(reflection_dir);
    let next = current.unwrap_or(0).max(high_water);
    if current == Some(next) {
        return Ok(next);
    }
    std::fs::create_dir_all(reflection_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(reflection_dir, std::fs::Permissions::from_mode(0o700));
    }
    let target = reflection_run_id_mark_path(reflection_dir);
    let temp = reflection_dir.join(format!("{RUN_ID_MARK_FILE}.tmp-{}", random_uuid()));
    let json = serde_json::to_string(&RunIdMark {
        version: 1,
        high_water: next,
    })
    .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if let Err(err) = write_temp_file(&temp, &format!("{json}\n")) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temp, &target) {
        let _ = std::fs::remove_file(&temp);
        return Err(err);
    }
    Ok(next)
}

fn write_temp_file(path: &Path, content: &str) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    let mut file = opts.open(path)?;
    file.write_all(content.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_run_numbers_and_advances_durable_high_water_monotonically() {
        assert_eq!(reflection_run_number("reflection-run-7"), 7);
        assert_eq!(reflection_run_number("reflection-run-12.json"), 12);
        assert_eq!(reflection_run_number("1755000000000-reflection-run-9"), 9);
        assert_eq!(reflection_run_number("other"), 0);

        let dir = tempfile::tempdir().unwrap();
        let reflection = dir.path().join("reflection");
        let sessions = dir.path().join("sessions");
        let worktrees = dir.path().join("worktrees");
        std::fs::create_dir_all(reflection.join("completions")).unwrap();
        std::fs::create_dir_all(reflection.join("runs/reflection-run-11")).unwrap();
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::create_dir_all(&worktrees).unwrap();
        std::fs::write(reflection.join("completions/reflection-run-4.json"), "{}").unwrap();
        std::fs::write(worktrees.join("1755000000000-reflection-run-14"), "").unwrap();

        let high = scan_reflection_run_high_water(&RunIdHighWaterPaths {
            reflection: reflection.clone(),
            reflection_sessions: sessions,
            worktrees,
        });
        assert_eq!(high, 14);
        assert_eq!(record_reflection_run_id_mark(&reflection, 14).unwrap(), 14);
        assert_eq!(record_reflection_run_id_mark(&reflection, 5).unwrap(), 14);
        assert_eq!(read_reflection_run_id_mark(&reflection), Some(14));
    }
}
