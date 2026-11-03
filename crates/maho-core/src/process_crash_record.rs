//! Port of senpi packages/coding-agent/src/core/process-crash-record.ts (paths, record shape, reader).
//!
//! The launch seam (`recordProcessLifetime`, the heartbeat marker and the dead-peer sweep) is ported in
//! `maho-cli/src/process_crash_record.rs`; this module carries the shared layout and the reader the
//! report and telemetry use.

use std::path::{Path, PathBuf};

use serde_json::Value;

pub const PROCESS_CRASH_RECORD_LIMIT: usize = 50;

/// How often a live process refreshes its marker, which bounds the error of a recorded uptime.
pub const PROCESS_MARKER_HEARTBEAT_MS: u64 = 60_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCrashPaths {
    pub dir: PathBuf,
    pub live_dir: PathBuf,
    pub records_file: PathBuf,
}

pub fn process_crash_paths(agent_dir: &Path) -> ProcessCrashPaths {
    let dir = agent_dir.join("process-crashes");
    ProcessCrashPaths {
        live_dir: dir.join("live"),
        records_file: dir.join("crashes.jsonl"),
        dir,
    }
}

/// Runtime identity every record carries: `senpiVersion`, and `productVersion` only when a branded build
/// version differs from it. There is no Bun runtime, so `bunVersion` is never set.
pub fn crashing_runtime_versions() -> serde_json::Map<String, Value> {
    let mut versions = serde_json::Map::new();
    let engine = env!("CARGO_PKG_VERSION");
    versions.insert("senpiVersion".to_owned(), Value::from(engine));
    let display = crate::engine_build_identity::ENGINE_VERSION;
    if display != engine {
        versions.insert("productVersion".to_owned(), Value::from(display));
    }
    versions
}

/// Every record currently held, oldest first. Unparseable lines are skipped, never failed on.
pub fn read_process_crash_records(agent_dir: &Path) -> Vec<Value> {
    let Ok(raw) = std::fs::read_to_string(process_crash_paths(agent_dir).records_file) else {
        return Vec::new();
    };
    raw.split('\n')
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("at").is_some_and(Value::is_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_well_formed_records() {
        let Some(dir) = tempfile::tempdir().ok() else {
            panic!("tempdir")
        };
        let paths = process_crash_paths(dir.path());
        assert_eq!(paths.live_dir, dir.path().join("process-crashes/live"));
        assert!(read_process_crash_records(dir.path()).is_empty());
        assert!(std::fs::create_dir_all(&paths.dir).is_ok());
        let lines =
            "{\"at\":\"2026-10-30T00:00:00Z\",\"kind\":\"print\"}\nnot json\n{\"kind\":\"x\"}\n\n";
        assert!(std::fs::write(&paths.records_file, lines).is_ok());
        let records = read_process_crash_records(dir.path());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["kind"], "print");
        assert!(crashing_runtime_versions().contains_key("senpiVersion"));
    }
}
