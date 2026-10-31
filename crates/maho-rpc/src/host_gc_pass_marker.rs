//! The record the budgeted gc pass keeps between passes, one per agent directory
//! (senpi `rpc/host-gc-pass-marker.ts`): when the last pass completed, where its rotation stopped, and
//! which endpoints proved uncollectable and are skipped until `skipUntil` (backoff 10 min -> 24 h).
//! Written by rename, so a reader sees the previous record or the next one; a record that does not parse
//! reads as "no pass has run yet".
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

pub const GC_SKIP_INITIAL_BACKOFF_MS: f64 = 10.0 * 60_000.0;
pub const GC_SKIP_MAX_BACKOFF_MS: f64 = 24.0 * 60.0 * 60_000.0;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostGcSkipEntry {
    pub skip_until: f64,
    pub backoff_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GcPassStoppedBy {
    Time,
    Count,
    Exhausted,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostGcPassMarker {
    pub completed_at: f64,
    pub cursor: Option<String>,
    pub examined: f64,
    pub removed: f64,
    pub stopped_by: GcPassStoppedBy,
    pub skip: BTreeMap<String, HostGcSkipEntry>,
}

pub fn host_gc_pass_marker_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("rpc-host-daemon").join("gc-last-run.json")
}

pub fn parse_host_gc_pass_marker(text: &str) -> Option<HostGcPassMarker> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    // `cursor` is required-nullable, as in the zod schema.
    if !value.as_object()?.contains_key("cursor") {
        return None;
    }
    serde_json::from_value(value).ok()
}

pub fn read_host_gc_pass_marker(agent_dir: &Path) -> io::Result<Option<HostGcPassMarker>> {
    match std::fs::read_to_string(host_gc_pass_marker_path(agent_dir)) {
        Ok(text) => Ok(parse_host_gc_pass_marker(&text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn write_host_gc_pass_marker(agent_dir: &Path, marker: &HostGcPassMarker) -> io::Result<()> {
    let path = host_gc_pass_marker_path(agent_dir);
    let staged = PathBuf::from(format!("{}.{}.tmp", path.display(), std::process::id()));
    let mut text = serde_json::to_string(marker).map_err(io::Error::other)?;
    text.push('\n');
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    std::io::Write::write_all(&mut options.open(&staged)?, text.as_bytes())?;
    std::fs::rename(staged, path)
}

/// The next skip for an endpoint that stayed uncollectable: 10 min first, doubling, capped at 24 h.
pub fn next_skip(previous: Option<&HostGcSkipEntry>, now: f64) -> HostGcSkipEntry {
    let backoff_ms = previous.map_or(GC_SKIP_INITIAL_BACKOFF_MS, |previous| {
        (previous.backoff_ms * 2.0).min(GC_SKIP_MAX_BACKOFF_MS)
    });
    HostGcSkipEntry {
        skip_until: now + backoff_ms,
        backoff_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_ten_minutes_and_caps_at_a_day() {
        let first = next_skip(None, 1_000.0);
        assert_eq!(
            first,
            HostGcSkipEntry {
                skip_until: 1_000.0 + GC_SKIP_INITIAL_BACKOFF_MS,
                backoff_ms: GC_SKIP_INITIAL_BACKOFF_MS
            }
        );
        assert_eq!(
            next_skip(Some(&first), 0.0).backoff_ms,
            2.0 * GC_SKIP_INITIAL_BACKOFF_MS
        );
        let capped = HostGcSkipEntry {
            skip_until: 0.0,
            backoff_ms: GC_SKIP_MAX_BACKOFF_MS,
        };
        assert_eq!(
            next_skip(Some(&capped), 0.0).backoff_ms,
            GC_SKIP_MAX_BACKOFF_MS
        );
    }

    #[test]
    fn marker_round_trips_and_garbage_reads_as_no_pass() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("rpc-host-daemon")).expect("mkdir");
        assert_eq!(read_host_gc_pass_marker(dir.path()).expect("read"), None);
        let marker = HostGcPassMarker {
            completed_at: 5.0,
            cursor: None,
            examined: 2.0,
            removed: 1.0,
            stopped_by: GcPassStoppedBy::Count,
            skip: BTreeMap::from([("abc".to_owned(), next_skip(None, 0.0))]),
        };
        write_host_gc_pass_marker(dir.path(), &marker).expect("write");
        assert_eq!(
            read_host_gc_pass_marker(dir.path()).expect("read"),
            Some(marker)
        );
        assert_eq!(parse_host_gc_pass_marker("{"), None);
        assert_eq!(
            parse_host_gc_pass_marker(
                r#"{"completedAt":1,"examined":0,"removed":0,"stoppedBy":"time","skip":{}}"#
            ),
            None
        );
    }
}
