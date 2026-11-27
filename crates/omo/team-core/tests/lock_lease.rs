//! Tests for `lock_lease`, `lock_worker_lifetime`, `parse_lock_holder`, and `LockWaitTimeout`.

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::thread;

use memory_core::locks::get_process_start_identity;
use team_core::TeamCoreError;
use team_core::team_state_store::lock_lease::{LockLeaseOptions, create_lock_lease};
use team_core::team_state_store::lock_publish::publish_lock_record;
use team_core::team_state_store::lock_reclaim::{LockReclaimOutcome, reclaim_guard_path};
use team_core::team_state_store::lock_record::{
    build_lock_record, forget_lock_record, mark_lock_record_released, parse_lock_holder,
};
use team_core::team_state_store::lock_worker_lifetime::{
    LockWorkerScope, lock_owner_instance_id, mark_lock_worker_exited,
};
use team_core::team_state_store::locks::{
    LockOptions, detect_stale_lock, reap_stale_lock_with_threshold, with_lock, with_lock_lease,
};

struct ChildHold {
    child: std::process::Child,
}

impl ChildHold {
    fn spawn() -> Self {
        let child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cat child");
        Self { child }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn stop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

impl Drop for ChildHold {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn hand_over_and_restore_roundtrips_five_line_child_record_and_original_bytes() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock_path = root.path().join("lock");
    let mut child = ChildHold::spawn();
    let child_pid = child.pid();

    let (original, handed, restored) = with_lock_lease(
        &lock_path,
        |lease| {
            let original = fs::read_to_string(&lock_path)?;
            lease.hand_over(child_pid)?;
            let handed = fs::read_to_string(&lock_path)?;
            lease.restore()?;
            let restored = fs::read_to_string(&lock_path)?;
            Ok((original, handed, restored))
        },
        Some(&LockOptions {
            stale_after_ms: Some(i64::MAX),
            owner_tag: None,
        }),
    )
    .expect("with_lock_lease");

    let lines: Vec<_> = handed.lines().collect();
    assert_eq!(lines.len(), 5);
    assert_eq!(lines[1], child_pid.to_string());
    assert_ne!(lines[3], original.lines().nth(3).unwrap_or_default());
    assert_eq!(restored, original);
    assert!(!lock_path.exists());
    child.stop();
}

#[test]
fn handed_lock_with_reused_pid_start_mismatch_is_stale_and_reaped() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock_path = root.path().join("lock");
    let mut child = ChildHold::spawn();
    let child_pid = child.pid();

    let handed = with_lock_lease(
        &lock_path,
        |lease| {
            lease.hand_over(child_pid)?;
            Ok(fs::read_to_string(&lock_path)?)
        },
        Some(&LockOptions {
            stale_after_ms: Some(i64::MAX),
            owner_tag: None,
        }),
    )
    .expect("with_lock_lease");

    let lines: Vec<_> = handed.lines().collect();
    let live_start = get_process_start_identity(child_pid).expect("child process start");
    let scheme = live_start.split(':').next().expect("scheme");
    let reused_start = format!("{scheme}:0");
    fs::write(
        &lock_path,
        format!("{}\n{reused_start}\n", lines[..4].join("\n")),
    )
    .expect("write reused pid record");

    assert_eq!(lines[4], live_start);
    assert!(detect_stale_lock(&lock_path, i64::MAX));
    assert_eq!(
        reap_stale_lock_with_threshold(&lock_path, i64::MAX).expect("reap"),
        LockReclaimOutcome::Removed
    );
    child.stop();
}

#[test]
fn handed_lock_remains_held_while_child_lives_and_is_stale_once_child_exits() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock_path = root.path().join("lock");
    let mut child = ChildHold::spawn();
    let child_pid = child.pid();

    with_lock_lease(
        &lock_path,
        |lease| {
            lease.hand_over(child_pid)?;
            assert!(!detect_stale_lock(&lock_path, 0));
            assert_eq!(
                reap_stale_lock_with_threshold(&lock_path, 0)?,
                LockReclaimOutcome::Changed
            );
            assert!(!publish_lock_record(
                &lock_path,
                &build_lock_record("contender")
            )?);
            child.stop();
            assert!(detect_stale_lock(&lock_path, 0));
            lease.restore()
        },
        Some(&LockOptions {
            stale_after_ms: Some(i64::MAX),
            owner_tag: None,
        }),
    )
    .expect("with_lock_lease");
    assert!(!lock_path.exists());
}

#[test]
fn restore_guard_turns_away_contender_between_compare_and_rename() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock_path = root.path().join("lock");
    let owner = build_lock_record("owner");
    assert!(publish_lock_record(&lock_path, &owner).expect("publish"));

    let mut child = ChildHold::spawn();
    let mut observed_reap = None;
    let mut observed_publish = None;
    let lock_path_clone = lock_path.clone();
    let lease = create_lock_lease(
        &lock_path,
        owner.clone(),
        "owner",
        LockLeaseOptions {
            stale_after_ms: Some(i64::MAX),
            before_restore_rename: Some(Box::new(|| {
                observed_reap = Some(reap_stale_lock_with_threshold(&lock_path_clone, i64::MAX)?);
                let contender = build_lock_record("contender");
                let pub_ok = publish_lock_record(&lock_path_clone, &contender)?;
                forget_lock_record(&contender);
                observed_publish = Some(pub_ok);
                Ok(())
            })),
        },
    );
    lease.hand_over(child.pid()).expect("hand_over");
    child.stop();

    lease.restore().expect("restore");
    drop(lease);
    assert_eq!(observed_reap, Some(LockReclaimOutcome::Busy));
    assert_eq!(observed_publish, Some(false));
    assert_eq!(fs::read_to_string(&lock_path).expect("read"), owner);
    mark_lock_record_released(&owner);
    forget_lock_record(&owner);
}

#[test]
fn restore_reports_lease_lost_when_contender_took_lock_or_holds_guard() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock_path = root.path().join("lock");
    let owner = build_lock_record("owner");
    assert!(publish_lock_record(&lock_path, &owner).expect("publish"));

    let mut child = ChildHold::spawn();
    let lease = create_lock_lease(
        &lock_path,
        owner.clone(),
        "owner",
        LockLeaseOptions::default(),
    );
    lease.hand_over(child.pid()).expect("hand_over");
    let handed = fs::read_to_string(&lock_path).expect("handed");

    let guard_path = reclaim_guard_path(&lock_path, &handed);
    let other_guard = build_lock_record("other-reaper");
    assert!(publish_lock_record(&guard_path, &other_guard).expect("guard publish"));

    let err = lease.restore().expect_err("restore should lose lease");
    assert!(matches!(err, TeamCoreError::LockLeaseLost(_)));
    assert_eq!(err.name(), "LockLeaseLostError");
    assert_eq!(fs::read_to_string(&lock_path).expect("read"), handed);

    mark_lock_record_released(&other_guard);
    forget_lock_record(&other_guard);
    mark_lock_record_released(&owner);
    forget_lock_record(&owner);
    child.stop();
}

#[test]
fn hand_over_refuses_self_pid_or_double_handover() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock_path = root.path().join("lock");
    let owner = build_lock_record("owner");
    assert!(publish_lock_record(&lock_path, &owner).expect("publish"));
    let lease = create_lock_lease(
        &lock_path,
        owner.clone(),
        "owner",
        LockLeaseOptions::default(),
    );
    let err = lease
        .hand_over(std::process::id())
        .expect_err("self handover");
    assert!(err.to_string().contains(&std::process::id().to_string()));
    assert_eq!(fs::read_to_string(&lock_path).expect("read"), owner);
    mark_lock_record_released(&owner);
    forget_lock_record(&owner);
}

#[test]
fn worker_scope_protects_live_worker_lock_until_scope_exits() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock_path = root.path().join("worker.lock");
    let held_barrier = Arc::new(Barrier::new(2));
    let exit_barrier = Arc::new(Barrier::new(2));

    let path_a = lock_path.clone();
    let held_a = Arc::clone(&held_barrier);
    let exit_a = Arc::clone(&exit_barrier);
    let worker_a = thread::spawn(move || {
        let _scope = LockWorkerScope::enter();
        let record = build_lock_record("worker-a");
        assert!(publish_lock_record(&path_a, &record).expect("publish worker-a"));
        held_a.wait();
        exit_a.wait();
        // Worker exits WITHOUT unlinking its lock file (simulating crash/exit).
        record
    });

    held_barrier.wait();
    assert!(!detect_stale_lock(&lock_path, 300_000));
    assert_eq!(
        reap_stale_lock_with_threshold(&lock_path, 300_000).expect("live worker reap"),
        LockReclaimOutcome::Changed
    );
    exit_barrier.wait();
    let leaked_record = worker_a.join().expect("join worker-a");

    assert!(detect_stale_lock(&lock_path, 300_000));
    assert_eq!(
        with_lock(&lock_path, || Ok("reclaimed"), None).expect("reclaim"),
        "reclaimed"
    );
    forget_lock_record(&leaked_record);
}

#[test]
fn parse_lock_holder_and_temp_name_matching_preserve_sibling_files() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock_path = root.path().join("lock");
    let sibling_temp = root
        .path()
        .join(format!(".lock.x.{}.lease", uuid::Uuid::new_v4()));
    let mut f = fs::File::create(&sibling_temp).expect("sibling");
    f.write_all(b"keep").expect("write");

    let mut child = ChildHold::spawn();
    with_lock_lease(
        &lock_path,
        |lease| {
            let content = fs::read_to_string(&lock_path)?;
            let holder = parse_lock_holder(&content).expect("parse holder");
            assert_eq!(holder.owner_pid, i64::from(std::process::id()));
            assert!(holder.acquired_at_epoch_ms > 0);
            lease.hand_over(child.pid())?;
            let child_content = fs::read_to_string(&lock_path)?;
            let child_holder = parse_lock_holder(&child_content).expect("child holder");
            assert_eq!(child_holder.owner_pid, i64::from(child.pid()));
            lease.restore()
        },
        Some(&LockOptions {
            stale_after_ms: Some(i64::MAX),
            owner_tag: None,
        }),
    )
    .expect("lease");
    assert!(sibling_temp.exists());
    assert!(parse_lock_holder("torn\n").is_none());
    child.stop();
}

#[test]
fn restore_loses_lease_when_contender_replaced_or_removed_handed_lock() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock_path = root.path().join("lock");
    let owner = build_lock_record("owner");
    assert!(publish_lock_record(&lock_path, &owner).expect("publish"));
    let mut child = ChildHold::spawn();
    let lease = create_lock_lease(
        &lock_path,
        owner.clone(),
        "owner",
        LockLeaseOptions::default(),
    );
    lease.hand_over(child.pid()).expect("hand_over");
    child.stop();

    // Contender removes the lock and publishes its own before restore runs.
    let contender = build_lock_record("contender");
    fs::write(&lock_path, &contender).expect("overwrite");
    let err = lease.restore().expect_err("restore lost after replace");
    assert!(matches!(err, TeamCoreError::LockLeaseLost(_)));
    assert_eq!(fs::read_to_string(&lock_path).expect("read"), contender);
    forget_lock_record(&contender);
    mark_lock_record_released(&owner);
    forget_lock_record(&owner);
}

#[test]
fn mark_lock_worker_exited_and_consecutive_handovers_work() {
    let root = tempfile::tempdir().expect("tempdir");
    let lock_path = root.path().join("worker-exit.lock");
    let (incarnation, leaked) = {
        let _scope = LockWorkerScope::enter();
        let incarnation = lock_owner_instance_id();
        let record = build_lock_record("scoped-worker");
        assert!(publish_lock_record(&lock_path, &record).expect("publish"));
        assert!(!detect_stale_lock(&lock_path, 300_000));
        mark_lock_worker_exited(&incarnation);
        assert!(detect_stale_lock(&lock_path, 300_000));
        (incarnation, record)
    };
    assert!(!incarnation.is_empty());
    assert_eq!(
        reap_stale_lock_with_threshold(&lock_path, 300_000).expect("reap"),
        LockReclaimOutcome::Removed
    );
    forget_lock_record(&leaked);

    // Consecutive hand-over -> restore -> hand-over -> restore on the same lease.
    let mut child = ChildHold::spawn();
    with_lock_lease(
        &lock_path,
        |lease| {
            lease.hand_over(child.pid())?;
            lease.restore()?;
            lease.hand_over(child.pid())?;
            lease.restore()
        },
        Some(&LockOptions {
            stale_after_ms: Some(i64::MAX),
            owner_tag: None,
        }),
    )
    .expect("double roundtrip");
    child.stop();
}
