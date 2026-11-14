//! Regressions for the sidecar reclaimer: which directories die, which survive, and which failures
//! are silent. Age is injected, never slept for, so every case is decided by the fixture.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::*;

const NOW: i64 = 1_800_000_000_000;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

struct Fixture {
    root: tempfile::TempDir,
    recall: PathBuf,
    locks: PathBuf,
    warnings: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let recall = root.path().join("recall");
        let locks = root.path().join("locks");
        std::fs::create_dir_all(sidecars_dir(&recall)).unwrap();
        std::fs::create_dir_all(&locks).unwrap();
        Self {
            root,
            recall,
            locks,
            warnings: Arc::new(Mutex::new(Vec::new())),
        }
    }
    fn warn_fn(&self) -> Arc<dyn Fn(&str) + Send + Sync> {
        let warnings = Arc::clone(&self.warnings);
        Arc::new(move |message: &str| {
            warnings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(message.to_string())
        })
    }

    fn warnings(&self) -> Vec<String> {
        self.warnings
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// A sidecar directory whose activity is `age_ms` old.
    fn sidecar(&self, session: &str, age_ms: i64) -> PathBuf {
        let dir = sidecar_dir(&self.recall, session);
        std::fs::create_dir_all(&dir).unwrap();
        set_age(&dir, age_ms);
        dir
    }

    fn sweep(&self, owned: Vec<String>) -> SweepReport {
        let warn = self.warn_fn();
        sweep_recall_sidecars(SweepOptions::new(
            &self.recall,
            &self.locks,
            NOW,
            owned,
            warn.as_ref(),
        ))
    }
}

/// Backdates a path by `age_ms` relative to `NOW`, through the real filesystem clock so the
/// production `last_activity_ms` reads it.
fn set_age(path: &Path, age_ms: i64) {
    let target = std::time::UNIX_EPOCH
        + std::time::Duration::from_millis(u64::try_from(NOW - age_ms).unwrap());
    let stamp = filetime::FileTime::from_system_time(target);
    filetime::set_file_mtime(path, stamp).unwrap();
}

#[test]
fn an_expired_sidecar_dir_is_renamed_then_removed() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-expired", 8 * DAY_MS);

    let report = fixture.sweep(Vec::new());

    assert_eq!(report.pruned, vec![encode_session("s-expired")]);
    assert!(!dir.exists(), "the expired directory must be gone");
    assert!(
        !fixture
            .warnings()
            .iter()
            .any(|line| line.contains("failed")),
        "a clean prune must not warn: {:?}",
        fixture.warnings()
    );
    let leftover: Vec<_> = std::fs::read_dir(sidecars_dir(&fixture.recall))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name())
        .collect();
    assert!(
        leftover.is_empty(),
        "no tombstone may survive a successful removal: {leftover:?}"
    );
}

#[test]
fn a_young_sidecar_dir_is_kept() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-young", 6 * DAY_MS);

    let report = fixture.sweep(Vec::new());

    assert!(report.pruned.is_empty());
    assert_eq!(report.kept, vec![encode_session("s-young")]);
    assert!(dir.exists());
}

#[test]
fn a_dir_whose_child_was_touched_recently_is_kept() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-active", 30 * DAY_MS);
    // Creating the child bumps the DIRECTORY's mtime, so the directory is re-aged afterwards: the
    // child alone must carry the activity.
    let transcript = dir.join("20260101T000000000Z_child.jsonl");
    std::fs::write(&transcript, b"{}\n").unwrap();
    set_age(&transcript, DAY_MS);
    set_age(&dir, 30 * DAY_MS);

    let report = fixture.sweep(Vec::new());

    assert!(
        report.pruned.is_empty(),
        "a recent direct child keeps the directory alive"
    );
    assert!(dir.exists());
}

#[test]
fn the_child_transcript_is_a_direct_child_of_the_sidecar_dir() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-shape", DAY_MS);
    let transcript = dir.join("20260101T000000000Z_abc.jsonl");
    std::fs::write(&transcript, b"{}\n").unwrap();

    let parent = transcript.parent().expect("the transcript has a parent");
    assert_eq!(
        parent, dir,
        "the activity basis is one level deep, so the transcript must be a DIRECT child"
    );
}

#[test]
fn an_owned_sidecar_dir_is_never_pruned_even_when_expired() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-owned", 30 * DAY_MS);
    let encoded = encode_session("s-owned");

    let report = fixture.sweep(vec![encoded.clone()]);

    assert!(report.pruned.is_empty());
    assert_eq!(report.kept, vec![encoded]);
    assert!(dir.exists(), "an owned directory is untouchable");
}

#[test]
fn a_name_outside_the_encoded_alphabet_is_kept_in_neither_list() {
    let fixture = Fixture::new();
    let stray = sidecars_dir(&fixture.recall).join("not.a.base64url.name");
    std::fs::create_dir_all(&stray).unwrap();
    set_age(&stray, 30 * DAY_MS);

    let report = fixture.sweep(Vec::new());

    assert!(report.pruned.is_empty());
    assert!(
        report.kept.is_empty(),
        "a name this code never wrote is neither pruned nor reported as kept"
    );
    assert!(stray.exists());
}

#[test]
#[cfg(unix)]
fn a_symlink_entry_is_kept() {
    let fixture = Fixture::new();
    let target = fixture.root.path().join("outside-target");
    std::fs::create_dir_all(&target).unwrap();
    set_age(&target, 30 * DAY_MS);
    let link = sidecars_dir(&fixture.recall).join(encode_session("s-link"));
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let report = fixture.sweep(Vec::new());

    assert!(
        report.pruned.is_empty(),
        "lstat sees a symlink, not the directory it points at"
    );
    assert!(target.exists(), "the link target must survive");
    assert!(link.symlink_metadata().is_ok());
}

#[test]
fn a_future_mtime_is_not_expired() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-future", -5 * DAY_MS);

    let report = fixture.sweep(Vec::new());

    assert!(
        report.pruned.is_empty(),
        "a future mtime yields a negative age and must never read as expired"
    );
    assert!(dir.exists());
}

#[test]
fn a_missing_sidecars_dir_is_silent() {
    let fixture = Fixture::new();
    std::fs::remove_dir_all(sidecars_dir(&fixture.recall)).unwrap();

    let report = fixture.sweep(Vec::new());

    assert!(report.pruned.is_empty());
    assert!(report.kept.is_empty());
    assert!(
        fixture.warnings().is_empty(),
        "a first run has no tree yet: {:?}",
        fixture.warnings()
    );
}

#[test]
fn an_existing_tombstone_is_removed_without_a_lock() {
    let fixture = Fixture::new();
    let tombstone = sidecars_dir(&fixture.recall).join(".prune-abc-1234");
    std::fs::create_dir_all(&tombstone).unwrap();
    std::fs::write(tombstone.join("leftover.jsonl"), b"{}\n").unwrap();
    set_age(&tombstone, 0);

    let report = fixture.sweep(Vec::new());

    assert!(
        !tombstone.exists(),
        "a tombstone is retried with no age check and no lock"
    );
    assert!(report.pruned.is_empty(), "cleanup is not a prune");
}

#[test]
fn the_post_lock_recheck_keeps_a_dir_that_became_fresh_during_the_claim() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-race", 30 * DAY_MS);
    let warn = fixture.warn_fn();
    let touched = dir.clone();
    // The claim's window: a wake appends to the transcript right after the lock is taken.
    let after_lock = move |path: &Path| {
        let transcript = path.join("20260101T000000000Z_race.jsonl");
        std::fs::write(&transcript, b"{}\n").unwrap();
        set_age(&transcript, DAY_MS);
        set_age(&touched, 30 * DAY_MS);
    };

    let mut options = SweepOptions::new(
        &fixture.recall,
        &fixture.locks,
        NOW,
        Vec::new(),
        warn.as_ref(),
    );
    options.after_lock = &after_lock;
    let report = sweep_recall_sidecars(options);

    assert!(
        report.pruned.is_empty(),
        "the post-lock re-check must see the wake that landed during the claim"
    );
    assert!(dir.exists());
}

#[test]
fn a_sweep_that_loses_the_claim_race_keeps_the_dir_and_is_silent() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-lost", 30 * DAY_MS);
    let warn = fixture.warn_fn();
    let lost = |_from: &Path, _to: &Path| Err(std::io::Error::from(std::io::ErrorKind::NotFound));

    let mut options = SweepOptions::new(
        &fixture.recall,
        &fixture.locks,
        NOW,
        Vec::new(),
        warn.as_ref(),
    );
    options.rename = &lost;
    let report = sweep_recall_sidecars(options);

    assert!(report.pruned.is_empty());
    assert!(dir.exists());
    assert!(
        fixture.warnings().is_empty(),
        "losing a claim to another process is expected, not a warning: {:?}",
        fixture.warnings()
    );
}

#[test]
fn a_dead_owner_lock_is_reclaimed_and_the_expired_dir_pruned() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-dead", 30 * DAY_MS);
    let encoded = encode_session("s-dead");
    let lock_path = sidecar_owner_lock(&fixture.locks, &encoded);
    // A child that has already exited: its recorded pid cannot be alive.
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();
    let record = memory_core::locks::lock_record::LockRecord {
        pid: dead_pid,
        process_start: "0".to_string(),
        hostname: memory_core::support::host::hostname(),
        nonce: memory_core::support::random::random_uuid(),
        created_at: memory_core::support::time::now_iso(),
        purpose: "recall-sidecar".to_string(),
        run_id: None,
        git_activity_lock: None,
    };
    std::fs::write(
        &lock_path,
        serde_json::to_string(&record).expect("the record serializes"),
    )
    .unwrap();

    let report = fixture.sweep(Vec::new());

    assert_eq!(report.pruned, vec![encoded]);
    assert!(!dir.exists());
}

#[test]
fn a_live_owner_lock_makes_the_claim_silent_and_keeps_the_dir() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-live", 30 * DAY_MS);
    let encoded = encode_session("s-live");
    let lock_path = sidecar_owner_lock(&fixture.locks, &encoded);
    let record = memory_core::locks::lock_record::create_lock_record(
        "recall-sidecar",
        memory_core::locks::lock_record::CreateLockRecordOptions::default(),
    )
    .unwrap();
    std::fs::write(&lock_path, serde_json::to_string(&record).unwrap()).unwrap();

    let report = fixture.sweep(Vec::new());

    assert!(report.pruned.is_empty());
    assert_eq!(report.kept, vec![encoded]);
    assert!(dir.exists(), "a live owner keeps its directory");
    assert!(
        fixture.warnings().is_empty(),
        "contention is the expected outcome, never a warning: {:?}",
        fixture.warnings()
    );
}

#[test]
fn a_failed_claim_warns_and_leaves_the_dir_in_place() {
    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-stuck", 30 * DAY_MS);
    let encoded = encode_session("s-stuck");
    // A rename onto an existing non-directory fails with ENOTDIR: neither a lost race nor a success,
    // so the claim must warn and change nothing.
    let tombstone = sidecars_dir(&fixture.recall).join(format!(".prune-{encoded}-fixed"));
    std::fs::write(&tombstone, b"occupied").unwrap();
    let warn = fixture.warn_fn();
    let fixed_uuid = || "fixed".to_string();

    let mut options = SweepOptions::new(
        &fixture.recall,
        &fixture.locks,
        NOW,
        Vec::new(),
        warn.as_ref(),
    );
    options.uuid = &fixed_uuid;
    let report = sweep_recall_sidecars(options);

    assert!(
        fixture
            .warnings()
            .iter()
            .any(|line| line.contains("sweep claim failed")),
        "a rename onto an occupied path must warn: {:?}",
        fixture.warnings()
    );
    assert!(report.pruned.is_empty());
    assert!(dir.exists(), "a failed claim leaves the directory in place");
}

#[test]
#[cfg(unix)]
fn a_removal_failure_warns_and_leaves_the_tombstone() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new();
    let dir = fixture.sidecar("s-unremovable", 30 * DAY_MS);
    let blocked = dir.join("blocked");
    std::fs::create_dir_all(&blocked).unwrap();
    std::fs::write(blocked.join("kept.jsonl"), b"{}\n").unwrap();
    // Removing `kept.jsonl` needs write permission on `blocked`, so the recursive removal fails
    // after the claim itself succeeded.
    let mut permissions = std::fs::metadata(&blocked).unwrap().permissions();
    permissions.set_mode(0o500);
    std::fs::set_permissions(&blocked, permissions).unwrap();

    let report = fixture.sweep(Vec::new());

    assert!(
        fixture
            .warnings()
            .iter()
            .any(|line| line.contains("sweep removal failed")),
        "an unremovable tree must warn: {:?}",
        fixture.warnings()
    );
    assert!(report.pruned.is_empty(), "a failed removal is not a prune");
    let tombstones: Vec<String> = std::fs::read_dir(sidecars_dir(&fixture.recall))
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        tombstones.len(),
        1,
        "the tombstone stays for the next sweep to retry: {tombstones:?}"
    );
    assert!(tombstones[0].starts_with(".prune-"));

    // The claim renamed the tree, so the blocked directory now lives under the tombstone.
    let moved = sidecars_dir(&fixture.recall)
        .join(&tombstones[0])
        .join("blocked");
    let mut permissions = std::fs::metadata(&moved).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&moved, permissions).unwrap();
}

#[test]
fn the_owner_lock_name_matches_the_name_on_disk() {
    let locks = Path::new("/m/agents/a/runtime/locks");
    assert_eq!(
        sidecar_owner_lock(locks, "MDFhMWJlMjctYzA3Mi03MjkzLWJmMzEtZmUxM2ExOGJmMjMx"),
        Path::new(
            "/m/agents/a/runtime/locks/recall-sidecar.MDFhMWJlMjctYzA3Mi03MjkzLWJmMzEtZmUxM2ExOGJmMjMx.lock"
        )
    );
}

#[test]
fn is_expired_compares_signed_ages() {
    assert!(is_expired(
        Some(NOW - RECALL_SIDECAR_MAX_AGE_MS),
        NOW,
        RECALL_SIDECAR_MAX_AGE_MS
    ));
    assert!(!is_expired(
        Some(NOW - RECALL_SIDECAR_MAX_AGE_MS + 1),
        NOW,
        RECALL_SIDECAR_MAX_AGE_MS
    ));
    assert!(!is_expired(
        Some(NOW + DAY_MS),
        NOW,
        RECALL_SIDECAR_MAX_AGE_MS
    ));
    assert!(!is_expired(None, NOW, RECALL_SIDECAR_MAX_AGE_MS));
}
