//! Reclaims run/completion JSON temporaries a crashed writer left behind (pin `run-temporaries.ts`).
//!
//! Startup can overlap another session's writes. UUID-only legacy temporaries have no owner
//! identity, so give them a full day; PID-bearing temporaries also require confirmed death. The
//! caller decides which directories are scanned; this module never recurses.

use std::path::Path;

use memory_core::locks::ProcessLiveness;

use super::run_artifacts::unlink_run_artifact;

/// A full day: UUID-only temporaries carry no owner to probe.
pub const STRANDED_TEMP_MIN_AGE_MS: f64 = 24.0 * 60.0 * 60_000.0;

/// Removes stranded `*.json.tmp-*` temporaries older than the age gate whose recorded owner pid is
/// confirmed dead. A missing directory is an empty sweep; a missing file is a no-op (the unlink race
/// is swallowed by `unlink_run_artifact`); every other error propagates.
pub fn sweep_stranded_run_temporaries(
    directory: &Path,
    now_ms: f64,
    pid_liveness: &dyn Fn(u32) -> ProcessLiveness,
) -> Result<(), String> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if !entry.file_type().map_err(|error| error.to_string())?.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(owner) = stranded_temp_owner(&name) else {
            continue;
        };
        let path = entry.path();
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        let modified_ms = metadata
            .modified()
            .map_err(|error| error.to_string())?
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis() as f64;
        if modified_ms > now_ms - STRANDED_TEMP_MIN_AGE_MS {
            continue;
        }
        if let Some(pid) = owner
            && pid_liveness(pid) != ProcessLiveness::Dead
        {
            continue;
        }
        unlink_run_artifact(&path).map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// `\.json\.tmp-(?:(\d+)-)?[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$` against
/// the full entry name: `None` = no match, `Some(pid)` carries the recorded owner pid.
fn stranded_temp_owner(name: &str) -> Option<Option<u32>> {
    let suffix = name.rsplit_once(".json.tmp-")?.1;
    let mut groups = suffix.split('-');
    let first = groups.next()?;
    let rest: Vec<&str> = groups.collect();
    // `\d+` only: `parse::<u32>` would also accept a leading `+`.
    if rest.len() == 5
        && !first.is_empty()
        && first.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(pid) = first.parse::<u32>()
        && is_lowercase_uuid(&rest)
    {
        return Some(Some(pid));
    }
    let mut uuid_groups = Vec::with_capacity(5);
    uuid_groups.push(first);
    uuid_groups.extend_from_slice(&rest);
    (uuid_groups.len() == 5 && is_lowercase_uuid(&uuid_groups)).then_some(None)
}

/// The exact `[0-9a-f]` group lengths `8-4-4-4-12`: uppercase hex is rejected.
fn is_lowercase_uuid(groups: &[&str]) -> bool {
    const LENGTHS: [usize; 5] = [8, 4, 4, 4, 12];
    groups.len() == LENGTHS.len()
        && groups.iter().zip(LENGTHS).all(|(group, expected)| {
            group.len() == expected
                && group
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const UUID: &str = "0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b";

    fn write(dir: &Path, name: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"{}").expect("temporary file");
        path
    }

    #[test]
    fn an_aged_temporary_is_removed_and_only_matching_shapes_are_touched() {
        let root = tempfile::tempdir().expect("temp dir");
        let owned = write(root.path(), &format!("ledger.json.tmp-4242-{UUID}"));
        let legacy = write(root.path(), &format!("pending.json.tmp-{UUID}"));
        let durable = write(root.path(), "ledger.json");
        let uppercase = write(root.path(), "ledger.json.tmp-0F1E2D3C-4B5A-6978-8A9B-0C1D2E3F4A5B");
        let plus_pid = write(root.path(), &format!("ledger.json.tmp-+42-{UUID}"));
        let liveness = |_: u32| ProcessLiveness::Dead;

        sweep_stranded_run_temporaries(root.path(), 4_000_000_000_000.0, &liveness).expect("sweep");

        assert!(!owned.exists());
        assert!(!legacy.exists());
        assert!(durable.exists());
        assert!(uppercase.exists());
        assert!(plus_pid.exists());
    }

    #[test]
    fn a_live_or_unknown_owner_preserves_an_aged_temporary() {
        let root = tempfile::tempdir().expect("temp dir");
        let live = write(root.path(), &format!("ledger.json.tmp-4242-{UUID}"));
        let unknown = write(root.path(), &format!("ledger.json.tmp-4243-{UUID}"));
        let liveness = |pid: u32| {
            if pid == 4242 { ProcessLiveness::Alive } else { ProcessLiveness::Unknown }
        };

        sweep_stranded_run_temporaries(root.path(), 4_000_000_000_000.0, &liveness).expect("sweep");

        assert!(live.exists());
        assert!(unknown.exists());
    }

    #[test]
    fn a_fresh_temporary_is_preserved_and_a_missing_directory_is_an_empty_sweep() {
        let root = tempfile::tempdir().expect("temp dir");
        let fresh = write(root.path(), &format!("ledger.json.tmp-4242-{UUID}"));
        let liveness = |_: u32| ProcessLiveness::Dead;

        sweep_stranded_run_temporaries(root.path(), 0.0, &liveness).expect("sweep");
        assert!(fresh.exists());
        sweep_stranded_run_temporaries(&root.path().join("absent"), 4_000_000_000_000.0, &liveness)
            .expect("absent sweep");
    }

    #[test]
    fn the_predicate_rejects_uppercase_and_non_uuid_shapes() {
        assert_eq!(stranded_temp_owner("a.json.tmp-0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b"), Some(None));
        assert_eq!(stranded_temp_owner("a.json.tmp-4242-0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b"), Some(Some(4242)));
        assert_eq!(stranded_temp_owner("a.json.tmp-0F1E2D3C-4B5A-6978-8A9B-0C1D2E3F4A5B"), None);
        assert_eq!(stranded_temp_owner("a.json.tmp-+42-0f1e2d3c-4b5a-6978-8a9b-0c1d2e3f4a5b"), None);
        assert_eq!(stranded_temp_owner("a.json.tmp-0f1e2d3c4b5a69788a9b0c1d2e3f4a5b"), None);
    }
}
