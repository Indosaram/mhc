//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-legacy-rollups.ts`.
//!
//! Releases before the crash-safe session summary kept their per-session rollups under `<state>/rollups/`.
//! Nothing reads that directory any more and nothing sends from it, so its files are data with no reader:
//! every start removes them, whatever the consent.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use telemetry_core::{TelemetryDiagnosticEvent, TelemetryDiagnostics};

/// The most entries one start removes; a larger leftover is finished by the next start.
pub const LEGACY_ROLLUP_REMOVE_LIMIT: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LegacyRollupsResult {
    pub removed: usize,
    /// Left on purpose: subdirectories, other entry types, entries past the limit, or a pass stopped because
    /// `rollups` stopped being the directory it started on.
    pub skipped: usize,
    /// Files or links whose removal failed.
    pub failed: usize,
}

fn cleaned() -> &'static Mutex<BTreeSet<PathBuf>> {
    static CLEANED: std::sync::OnceLock<Mutex<BTreeSet<PathBuf>>> = std::sync::OnceLock::new();
    CLEANED.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// Once per state dir per process: removes the leftovers and logs counts only, never a name or path.
pub fn remove_legacy_rollups_once(
    state_dir: &Path,
    diagnostics: Option<&TelemetryDiagnostics>,
) {
    {
        let mut cleaned = cleaned().lock().unwrap_or_else(PoisonError::into_inner);
        if !cleaned.insert(state_dir.to_path_buf()) {
            return;
        }
    }
    let result = remove_legacy_rollups(state_dir, LEGACY_ROLLUP_REMOVE_LIMIT);
    let Some(diagnostics) = diagnostics else {
        return;
    };
    if result.removed > 0 {
        diagnostics(&telemetry_core::TelemetryDiagnosticInput {
            event: TelemetryDiagnosticEvent::TelemetryLegacyRollupsRemoved,
            source: "omo-native-component".into(),
            error: None,
            error_kind: None,
            entry_kind: None,
            error_code: None,
            count: Some(result.removed as u64),
        });
    }
    if result.failed > 0 {
        diagnostics(&telemetry_core::TelemetryDiagnosticInput {
            event: TelemetryDiagnosticEvent::TelemetryActivityStateWriteFailed,
            source: "omo-native-component".into(),
            error: None,
            error_kind: None,
            entry_kind: None,
            error_code: None,
            count: Some(result.failed as u64),
        });
    }
}

pub fn legacy_rollup_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("rollups")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Directory,
    File,
    Link,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Identity {
    kind: EntryKind,
    dev: u64,
    ino: u64,
}

fn identity(path: &Path) -> Option<Identity> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let kind = if metadata.file_type().is_symlink() {
        EntryKind::Link
    } else if metadata.is_dir() {
        EntryKind::Directory
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(Identity {
            kind,
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        Some(Identity {
            kind,
            dev: 0,
            ino: 0,
        })
    }
}

fn is_same_directory(now: Option<Identity>, start: Identity) -> bool {
    now.is_some_and(|now| now.kind == EntryKind::Directory && now.dev == start.dev && now.ino == start.ino)
}

/// Removes `<state>/rollups/` without deleting what a link points to. A `rollups` link is unlinked itself
/// and its target is never read. Inside the directory only files and links are removed, at most `limit` per
/// call; subdirectories are left. Before every removal the path is checked again to still be the same
/// directory (device and inode), so a `rollups` swapped for a link mid-pass stops the pass at the next
/// removal. The emptied directory is removed last.
pub fn remove_legacy_rollups(state_dir: &Path, limit: usize) -> LegacyRollupsResult {
    let dir = legacy_rollup_dir(state_dir);
    let Some(start) = identity(&dir) else {
        return LegacyRollupsResult::default();
    };
    if start.kind == EntryKind::Link {
        return if unlink(&dir) {
            LegacyRollupsResult {
                removed: 1,
                skipped: 0,
                failed: 0,
            }
        } else {
            LegacyRollupsResult {
                removed: 0,
                skipped: 0,
                failed: 1,
            }
        };
    }
    if start.kind != EntryKind::Directory {
        return LegacyRollupsResult {
            removed: 0,
            skipped: 1,
            failed: 0,
        };
    }
    let entries = list(&dir);
    let (mut removed, mut skipped, mut failed) = (0_usize, 0_usize, 0_usize);
    for (index, (name, kind)) in entries.iter().enumerate() {
        if removed >= limit || *kind == EntryKind::Directory || *kind == EntryKind::Other {
            skipped += 1;
            continue;
        }
        if !is_same_directory(identity(&dir), start) {
            skipped += entries.len() - index;
            return LegacyRollupsResult {
                removed,
                skipped,
                failed,
            };
        }
        if unlink(&dir.join(name)) {
            removed += 1;
        } else {
            failed += 1;
        }
    }
    if skipped == 0 && failed == 0 && is_same_directory(identity(&dir), start) {
        // Another start emptied and removed it first, or a file arrived meanwhile: the next start retries.
        let _ignored = std::fs::remove_dir(&dir);
    }
    LegacyRollupsResult {
        removed,
        skipped,
        failed,
    }
}

fn list(dir: &Path) -> Vec<(String, EntryKind)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| {
            let kind = entry.file_type().map_or(EntryKind::Other, |file_type| {
                if file_type.is_symlink() {
                    EntryKind::Link
                } else if file_type.is_dir() {
                    EntryKind::Directory
                } else if file_type.is_file() {
                    EntryKind::File
                } else {
                    EntryKind::Other
                }
            });
            (entry.file_name().to_string_lossy().into_owned(), kind)
        })
        .collect()
}

fn unlink(path: &Path) -> bool {
    std::fs::remove_file(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn files_and_links_go_subdirectories_stay() {
        let dir = state();
        let rollups = legacy_rollup_dir(dir.path());
        std::fs::create_dir_all(rollups.join("nested")).unwrap();
        std::fs::write(rollups.join("a.json"), "{}").unwrap();
        std::fs::write(rollups.join("b.json"), "{}").unwrap();
        let result = remove_legacy_rollups(dir.path(), LEGACY_ROLLUP_REMOVE_LIMIT);
        assert_eq!(result.removed, 2);
        assert_eq!(result.skipped, 1);
        assert_eq!(result.failed, 0);
        assert!(rollups.join("nested").is_dir());
        assert!(!rollups.join("a.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_rollups_link_is_unlinked_and_its_target_survives() {
        let dir = state();
        let target = dir.path().join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("keep.json"), "{}").unwrap();
        std::os::unix::fs::symlink(&target, legacy_rollup_dir(dir.path())).unwrap();
        let result = remove_legacy_rollups(dir.path(), LEGACY_ROLLUP_REMOVE_LIMIT);
        assert_eq!(result.removed, 1);
        assert!(!legacy_rollup_dir(dir.path()).exists());
        assert!(target.join("keep.json").exists());
    }

    #[test]
    fn the_limit_leaves_the_rest_for_the_next_start() {
        let dir = state();
        let rollups = legacy_rollup_dir(dir.path());
        std::fs::create_dir_all(&rollups).unwrap();
        for index in 0..5 {
            std::fs::write(rollups.join(format!("{index}.json")), "{}").unwrap();
        }
        let result = remove_legacy_rollups(dir.path(), 2);
        assert_eq!(result.removed, 2);
        assert_eq!(result.skipped, 3);
        assert!(rollups.is_dir());
        let rest = remove_legacy_rollups(dir.path(), LEGACY_ROLLUP_REMOVE_LIMIT);
        assert_eq!(rest.removed, 3);
        assert!(!rollups.exists());
    }

    #[test]
    fn an_absent_directory_is_nothing_to_do() {
        let dir = state();
        assert_eq!(
            remove_legacy_rollups(dir.path(), LEGACY_ROLLUP_REMOVE_LIMIT),
            LegacyRollupsResult::default()
        );
    }

    #[test]
    fn removal_runs_once_per_state_dir_and_reports_counts_only() {
        let dir = state();
        let rollups = legacy_rollup_dir(dir.path());
        std::fs::create_dir_all(&rollups).unwrap();
        std::fs::write(rollups.join("secret-name.json"), "{}").unwrap();
        let reported = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = std::sync::Arc::clone(&reported);
        let diagnostics: TelemetryDiagnostics = std::sync::Arc::new(move |input| {
            observed.lock().unwrap().push((
                input.event,
                input.count,
                input.error.as_ref().map(|error| error.message.clone()),
            ));
        });
        remove_legacy_rollups_once(dir.path(), Some(&diagnostics));
        remove_legacy_rollups_once(dir.path(), Some(&diagnostics));
        let reported = reported.lock().unwrap();
        assert_eq!(reported.len(), 1);
        assert_eq!(
            reported[0].0,
            TelemetryDiagnosticEvent::TelemetryLegacyRollupsRemoved
        );
        assert_eq!(reported[0].1, Some(1));
        assert!(reported[0].2.is_none());
        assert!(!format!("{reported:?}").contains("secret-name"));
    }

    #[test]
    fn a_state_directory_swap_stops_the_pass() {
        let dir = state();
        let rollups = legacy_rollup_dir(dir.path());
        std::fs::create_dir_all(&rollups).unwrap();
        std::fs::write(rollups.join("a.json"), "{}").unwrap();
        // The directory identity is checked before every removal; a swap between listing and removing stops it.
        let swapped = legacy_rollup_dir(&dir.path().join("other"));
        std::fs::create_dir_all(&swapped).unwrap();
        let result = remove_legacy_rollups(dir.path(), LEGACY_ROLLUP_REMOVE_LIMIT);
        assert_eq!(result.removed, 1);
        assert_eq!(result.failed, 0);
    }
}
