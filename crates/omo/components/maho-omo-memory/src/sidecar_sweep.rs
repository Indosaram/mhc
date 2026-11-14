//! The kibitzer recall-sidecar reclaimer (senpi `sidecar-sweep.ts`).
//!
//! A resident sidecar keeps a child transcript under `<recall>/sidecars/<base64url(session)>`. Those
//! directories were written by every past session and nothing ever removed them, so the tree grows
//! without bound: on this machine 825 directories / 151.6 MB, 781 of them older than a week. This
//! module is upstream's reclaim path, ported name-for-name.
//!
//! The rule is age-based: a directory whose newest activity is older than seven days is renamed to a
//! `.prune-` tombstone and removed. "Activity" is the directory's own `lstat` mtime or any DIRECT
//! child's, never a recursive walk - see `last_activity_ms` for why one level is enough.
//!
//! Reclaiming is the dangerous direction, so four guards stand between a live session and deletion:
//! the owned set (a session in this process is never reclaimed), the post-lock expiry re-check (a
//! wake that landed mid-sweep saves the directory), the encoded-name check (only names this code
//! wrote are candidates), and the long-held owner lock from `sidecar_owner` (the only guard that
//! survives a second process).
//!
//! Failures never propagate: a sweep runs on a background blocking pool, so an error is warned and
//! the directory is kept. There is no error list in the report, matching upstream.

use std::io;
use std::path::{Path, PathBuf};

use base64::Engine;
use memory_core::locks::acquire::{
    AcquireLockError, AcquireLockOptions, acquire_lock, release_lock,
};
use memory_core::locks::lock_record::{CreateLockRecordOptions, LockRecord, create_lock_record};

/// senpi `STE`: a sidecar directory is reclaimable once its newest activity is this old.
pub const RECALL_SIDECAR_MAX_AGE_MS: i64 = 7 * 24 * 60 * 60 * 1000;

/// The tombstone prefix: a claimed directory is renamed to `.prune-<name>-<uuid>` before removal, so
/// a crash between claim and removal leaves a name the next sweep retries unconditionally.
const PRUNE_PREFIX: &str = ".prune-";

/// The lock purpose recorded in the record. Operators grep it, so it matches the lock file name.
const LOCK_PURPOSE: &str = "recall-sidecar";

/// `<recall>/sidecars`.
pub fn sidecars_dir(recall: &Path) -> PathBuf {
    recall.join("sidecars")
}

/// senpi `sidecarSessionDir`: `<recall>/sidecars/<base64url(session)>`.
pub fn sidecar_dir(recall: &Path, session_id: &str) -> PathBuf {
    sidecars_dir(recall).join(encode_session(session_id))
}

/// The directory name for a session id: `base64url` without padding, byte-identical to what OmO
/// wrote for the same parent id.
pub fn encode_session(session_id: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(session_id)
}

/// `<locks>/recall-sidecar.<encoded>.lock` - ONE path, acquired twice (held by the owner for the
/// session's life, and briefly by a sweep claim).
pub fn sidecar_owner_lock(locks: &Path, encoded: &str) -> PathBuf {
    locks.join(format!("{LOCK_PURPOSE}.{encoded}.lock"))
}

/// `ENCODED_RE`: only `base64url` names are candidates. A directory this code did not name is never
/// claimed, so a stray or user-created directory survives the sweep.
fn is_encoded_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// senpi `lastActivity`: the directory's own mtime or any DIRECT child's, whichever is newer.
///
/// `lstat`, not `stat`: a symlink entry is not a directory and keeps its own mtime rather than its
/// target's. Every failed `lstat` is skipped instead of failing the check, so one unreadable child
/// cannot block reclamation of an otherwise dead tree.
///
/// One level is enough because the sidecar's child transcript (`<ts>_<id>.jsonl`) is a DIRECT child
/// and every wake appends to it. Deeper entries (`session-holders/`, `resident-blobs/`) are not read,
/// and would be irrelevant anyway since a wake always touches the transcript first.
///
/// `None` means "no observable activity", which is never treated as expired.
pub fn last_activity_ms(dir: &Path) -> Option<i64> {
    let mut newest = modified_ms(dir);
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if let Some(stamp) = modified_ms(&entry.path()) {
                newest = Some(newest.map_or(stamp, |current| current.max(stamp)));
            }
        }
    }
    newest
}

fn modified_ms(path: &Path) -> Option<i64> {
    std::fs::symlink_metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
}

/// senpi `isExpired`. Signed comparison: a directory with a future mtime yields a negative age and is
/// never expired. An unsigned subtraction would wrap to a huge value and reclaim it - the wrong
/// direction for the one operation that deletes data.
pub fn is_expired(last_activity_ms: Option<i64>, now_ms: i64, max_age_ms: i64) -> bool {
    last_activity_ms.is_some_and(|last| now_ms.saturating_sub(last) >= max_age_ms)
}

/// What one sweep did. Upstream logs the counts and nothing else; failures are warned, never returned.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SweepReport {
    /// Encoded names of the directories removed.
    pub pruned: Vec<String>,
    /// Encoded names of the directories that were eligible but survived.
    pub kept: Vec<String>,
}

/// What one sweep needs from its caller.
///
/// The seams exist because the interesting paths are otherwise unreachable in a test: the claim race
/// needs an injected `rename`, and the TOCTOU re-check needs a way to make the directory fresh after
/// the lock is taken.
pub struct SweepOptions<'a> {
    /// The identity's `runtime/recall` directory.
    pub recall_dir: &'a Path,
    /// The identity's `runtime/locks` directory.
    pub locks_dir: &'a Path,
    pub now_ms: i64,
    pub max_age_ms: i64,
    /// The encoded names this process currently owns. Never reclaimed.
    pub owned: Vec<String>,
    /// Warnings are logged; a sweep never fails a wake, a turn, or a shutdown.
    pub warn: &'a (dyn Fn(&str) + Send + Sync),
    /// senpi `rename`. Production is `std::fs::rename`; the claim-race test substitutes one that
    /// reports `NotFound` so the lost-race branch is exercised.
    pub rename: &'a (dyn Fn(&Path, &Path) -> io::Result<()> + Send + Sync),
    /// A new tombstone suffix. Production is `random_uuid`.
    pub uuid: &'a (dyn Fn() -> String + Send + Sync),
    /// Runs after the claim lock is held and BEFORE the expiry re-check. Production is a no-op; the
    /// TOCTOU test uses it to touch the directory as a concurrent wake would.
    pub after_lock: &'a (dyn Fn(&Path) + Send + Sync),
}

impl<'a> SweepOptions<'a> {
    /// Production seams: the real `rename`, a fresh uuid, and no post-lock hook.
    /// Production seams: the real `rename`, a fresh uuid, and no post-lock hook.
    pub fn new(
        recall_dir: &'a Path,
        locks_dir: &'a Path,
        now_ms: i64,
        owned: Vec<String>,
        warn: &'a (dyn Fn(&str) + Send + Sync),
    ) -> Self {
        Self {
            recall_dir,
            locks_dir,
            now_ms,
            max_age_ms: RECALL_SIDECAR_MAX_AGE_MS,
            owned,
            warn,
            rename: &|from: &Path, to: &Path| std::fs::rename(from, to),
            uuid: &memory_core::support::random::random_uuid,
            after_lock: &|_dir: &Path| {},
        }
    }
}

/// senpi `sweep`. Walks `<recall>/sidecars` once and returns what it removed and what it kept.
///
/// Ordering matters: tombstones are cleaned first and unconditionally (a crashed claim leaves one),
/// the owned set is consulted before any age check, and only a name this code wrote can be claimed.
pub fn sweep_recall_sidecars(options: SweepOptions<'_>) -> SweepReport {
    let mut report = SweepReport::default();
    let root = sidecars_dir(options.recall_dir);
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        // A first run has no sidecar tree at all; that is not a failure worth a line.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return report,
        Err(error) => {
            (options.warn)(&format!(
                "omo-senpi kibitzer sidecar sweep listing failed: {error}"
            ));
            return report;
        }
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let dir = entry.path();
        if name.starts_with(PRUNE_PREFIX) {
            remove_tombstone(&dir, options.warn);
            continue;
        }
        if options.owned.iter().any(|owned| owned == &name) {
            report.kept.push(name);
            continue;
        }
        if !is_encoded_name(&name) {
            continue;
        }
        if !std::fs::symlink_metadata(&dir).is_ok_and(|meta| meta.is_dir()) {
            continue;
        }
        let lock_path = sidecar_owner_lock(options.locks_dir, &name);
        if is_expired(last_activity_ms(&dir), options.now_ms, options.max_age_ms)
            && claim_and_remove(&dir, &lock_path, &name, &options)
        {
            report.pruned.push(name);
        } else {
            report.kept.push(name);
        }
    }
    report.pruned.sort();
    report.kept.sort();
    report
}

/// senpi `claimAndRemove`: take a non-blocking claim on the directory's owner lock, re-check the age,
/// rename to a tombstone, remove, release.
///
/// The wait timeout is 0: a live sidecar's owner lock is held for the session's life, and waiting on it
/// would stall the sweep for a session that is not going to end. The claim is silent on contention
/// because it is the expected outcome whenever another host is running.
fn claim_and_remove(
    dir: &Path,
    lock_path: &Path,
    encoded: &str,
    options: &SweepOptions<'_>,
) -> bool {
    let record: LockRecord = match create_lock_record(
        LOCK_PURPOSE,
        CreateLockRecordOptions {
            run_id: Some(encoded.to_string()),
        },
    ) {
        Ok(record) => record,
        Err(error) => {
            (options.warn)(&format!(
                "omo-senpi kibitzer sidecar sweep lock failed: {error}"
            ));
            return false;
        }
    };
    let acquire_options = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        retry_delay_ms: None,
        cancellation: None,
    };
    match acquire_lock(lock_path, &record, &acquire_options) {
        Ok(()) => {}
        Err(AcquireLockError::Contention(_)) => return false,
        Err(error) => {
            (options.warn)(&format!(
                "omo-senpi kibitzer sidecar sweep lock failed: {error}"
            ));
            return false;
        }
    }
    let claimed = claim_locked(dir, encoded, options);
    if let Err(error) = release_lock(lock_path, &record) {
        (options.warn)(&format!(
            "omo-senpi kibitzer sidecar sweep lock release failed: {error}"
        ));
    }
    claimed
}

/// The half of the claim that runs while the lock is held.
fn claim_locked(dir: &Path, encoded: &str, options: &SweepOptions<'_>) -> bool {
    (options.after_lock)(dir);
    // Guard 2: the directory was stat-ed before the lock, and a wake may have landed since. Re-read
    // under the lock so the newest activity, not the pre-lock reading, decides.
    if !is_expired(last_activity_ms(dir), options.now_ms, options.max_age_ms) {
        return false;
    }
    let Some(parent) = dir.parent() else {
        return false;
    };
    let tombstone = parent.join(format!(
        "{PRUNE_PREFIX}{encoded}-{}",
        (options.uuid)().as_str()
    ));
    match (options.rename)(dir, &tombstone) {
        Ok(()) => {}
        // Another process claimed it between the claim and the rename.
        Err(error) if error.kind() == io::ErrorKind::NotFound => return false,
        Err(error) => {
            (options.warn)(&format!(
                "omo-senpi kibitzer sidecar sweep claim failed: {error}"
            ));
            return false;
        }
    }
    remove_tombstone(&tombstone, options.warn)
}

/// Remove a claimed tree. `NotFound` is success: another process may have removed it already, which
/// is exactly what `remove_dir_all`'s "force" semantics make uninteresting.
fn remove_tombstone(dir: &Path, warn: &(dyn Fn(&str) + Send + Sync)) -> bool {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => {
            warn(&format!(
                "omo-senpi kibitzer sidecar sweep removal failed: {error}"
            ));
            false
        }
    }
}

#[cfg(test)]
#[path = "sidecar_sweep_tests.rs"]
mod tests;
