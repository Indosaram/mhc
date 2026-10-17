use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use crate::git::{
    GitMemoryRepo, KeptGitLock, KeptGitLockReason, StrandedGitLockError, UnlockFindings,
    UnlockInspection, UnlockOutcome, UnlockScans, UnlockVerdict, WriterState,
    inspect_stranded_git_lock, unlock_stranded_git_lock,
};
use crate::identity::layout::{MemoryIdentityPaths, build_identity_paths};
use crate::locks::{
    AcquireLockOptions, CreateLockRecordOptions, acquire_lock, create_lock_record,
    memory_writer_lock_path, release_lock,
};

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_live_process() -> ChildGuard {
    let child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn cat");
    ChildGuard(child)
}

fn spawn_dead_pid() -> u32 {
    let mut child = Command::new("true")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn true");
    let pid = child.id();
    let _ = child.wait();
    pid
}

fn nobody_scans() -> UnlockScans {
    UnlockScans {
        find_holders: Arc::new(|_| Some(Vec::new())),
        find_repo_users: Arc::new(|_| Some(Vec::new())),
    }
}

fn identity(agent_id: &str) -> (tempfile::TempDir, MemoryIdentityPaths) {
    let root = tempfile::tempdir().expect("tempdir");
    let canonical_root = std::fs::canonicalize(root.path()).unwrap_or_else(|_| root.path().to_path_buf());
    let paths = build_identity_paths(&canonical_root, agent_id);
    std::fs::create_dir_all(&paths.repo).expect("create repo");
    std::fs::create_dir_all(&paths.locks).expect("create locks");
    let repo = GitMemoryRepo::open(&paths.repo, agent_id).expect("open repo");
    repo.init(None).expect("init repo");
    (root, paths)
}

fn strand(repo_dir: &Path, relative: &str) -> PathBuf {
    let lock_path = repo_dir.join(".git").join(relative);
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(&lock_path, "").expect("write lock");
    lock_path
}

fn writer_record(paths: &MemoryIdentityPaths, pid: u32) {
    let json = serde_json::json!({
        "pid": pid,
        "process_start": "unavailable",
        "hostname": crate::support::host::hostname(),
        "nonce": crate::support::random::random_uuid(),
        "created_at": crate::support::time::now_iso(),
        "purpose": "memory-write",
    });
    std::fs::write(
        memory_writer_lock_path(&paths.locks),
        serde_json::to_string(&json).expect("serialize record"),
    )
    .expect("write writer lock");
}

#[test]
fn given_a_kept_lock_when_the_error_renders_then_it_names_the_path_reason_and_unlock_command() {
    let lock_path = PathBuf::from("/memory/agents/a/repo/.git/index.lock");
    let err = StrandedGitLockError::new(vec![KeptGitLock {
        path: lock_path.clone(),
        reason: KeptGitLockReason::RepoInUse,
    }]);
    assert!(err.message.contains("/memory/agents/a/repo/.git/index.lock"));
    assert!(err.message.contains(
        "a live process is still working in the repository, or that could not be ruled out"
    ));
    assert!(
        err.message
            .contains("/memory unlock /memory/agents/a/repo/.git/index.lock")
    );
}

#[test]
fn given_a_memory_writer_in_this_process_holding_the_memory_write_lock_when_inspected_and_unlocked_then_both_refuse_and_lock_stays()
{
    let (_root, paths) = identity("unlock-same-process-writer");
    let lock_path = strand(&paths.repo, "index.lock");
    let record =
        create_lock_record("memory-write", CreateLockRecordOptions::default()).expect("record");
    let writer_lock = memory_writer_lock_path(&paths.locks);
    acquire_lock(&writer_lock, &record, &AcquireLockOptions::default()).expect("acquire");

    let scans = nobody_scans();
    let inspection = inspect_stranded_git_lock(&paths.repo, &lock_path, Some(&scans), None);
    let UnlockInspection::Found(findings) = inspection else {
        panic!("expected Found");
    };
    let forced_confirm = UnlockFindings {
        verdict: UnlockVerdict::Confirm,
        ..findings.clone()
    };
    let outcome =
        unlock_stranded_git_lock(&paths.repo, &forced_confirm, Some(&scans)).expect("unlock");
    let _ = release_lock(&writer_lock, &record);

    assert_eq!(
        findings.writer,
        WriterState::Alive {
            pid: std::process::id()
        }
    );
    assert_eq!(findings.verdict, UnlockVerdict::Refuse);
    assert!(matches!(outcome, UnlockOutcome::Refused { .. }));
    assert!(lock_path.exists());
}

#[test]
fn given_an_unreadable_memory_write_lock_record_when_inspected_then_writer_is_reported_unknown_never_none()
{
    let (_root, paths) = identity("unlock-unreadable-writer");
    let lock_path = strand(&paths.repo, "index.lock");
    std::fs::write(memory_writer_lock_path(&paths.locks), "{not json").expect("corrupt lock");

    let scans = nobody_scans();
    let inspection = inspect_stranded_git_lock(&paths.repo, &lock_path, Some(&scans), None);
    let UnlockInspection::Found(findings) = inspection else {
        panic!("expected Found");
    };

    assert_eq!(findings.writer, WriterState::Unknown);
    assert_eq!(findings.verdict, UnlockVerdict::Refuse);
    assert_eq!(
        findings.writer_lock_path,
        Some(memory_writer_lock_path(&paths.locks))
    );
}

#[test]
fn given_a_path_that_is_not_a_git_lock_the_sweep_considers_when_inspected_then_it_is_rejected() {
    let (_root, paths) = identity("unlock-not-a-lock");
    let other = paths.repo.join("notes.lock");
    std::fs::write(&other, "").expect("write other");

    let scans = nobody_scans();
    let inspection = inspect_stranded_git_lock(&paths.repo, &other, Some(&scans), None);

    assert_eq!(inspection, UnlockInspection::NotAGitLock);
}

#[test]
fn given_a_process_visibly_holding_the_lock_open_when_inspected_then_verdict_is_refuse_and_holder_is_shown()
{
    let (_root, paths) = identity("unlock-holder");
    let lock_path = strand(&paths.repo, "index.lock");
    let scans = UnlockScans {
        find_holders: Arc::new(|_| Some(vec![4242])),
        find_repo_users: Arc::new(|_| Some(Vec::new())),
    };

    let inspection = inspect_stranded_git_lock(&paths.repo, &lock_path, Some(&scans), None);
    let UnlockInspection::Found(findings) = inspection else {
        panic!("expected Found");
    };

    assert_eq!(findings.verdict, UnlockVerdict::Refuse);
    assert_eq!(findings.holders, Some(vec![4242]));
}

#[test]
fn given_a_live_process_working_in_the_repository_when_unlock_runs_on_confirmed_inspection_then_it_refuses_and_lock_stays()
{
    let (_root, paths) = identity("unlock-repo-user");
    let lock_path = strand(&paths.repo, "index.lock");
    let nobody = nobody_scans();
    let UnlockInspection::Found(confirmed) =
        inspect_stranded_git_lock(&paths.repo, &lock_path, Some(&nobody), None)
    else {
        panic!("expected Found");
    };
    let now_busy = UnlockScans {
        find_holders: Arc::new(|_| Some(Vec::new())),
        find_repo_users: Arc::new(|_| Some(vec![5151])),
    };

    let outcome =
        unlock_stranded_git_lock(&paths.repo, &confirmed, Some(&now_busy)).expect("unlock");

    assert!(matches!(outcome, UnlockOutcome::Refused { .. }));
    assert!(lock_path.exists());
}

#[test]
fn given_a_live_memory_writer_recorded_in_the_writer_lock_when_inspected_then_verdict_is_refuse() {
    let (_root, paths) = identity("unlock-live-writer");
    let lock_path = strand(&paths.repo, "index.lock");
    let writer = spawn_live_process();
    let writer_pid = writer.0.id();
    writer_record(&paths, writer_pid);

    let scans = nobody_scans();
    let inspection = inspect_stranded_git_lock(&paths.repo, &lock_path, Some(&scans), None);
    let UnlockInspection::Found(findings) = inspection else {
        panic!("expected Found");
    };

    assert_eq!(findings.writer, WriterState::Alive { pid: writer_pid });
    assert_eq!(findings.verdict, UnlockVerdict::Refuse);
}

#[test]
fn given_a_dead_recorded_writer_and_no_visible_holder_when_inspected_then_verdict_is_confirm_never_automatic_removal()
{
    let (_root, paths) = identity("unlock-dead-writer");
    let lock_path = strand(&paths.repo, "index.lock");
    let dead_pid = spawn_dead_pid();
    writer_record(&paths, dead_pid);

    let scans = nobody_scans();
    let inspection = inspect_stranded_git_lock(&paths.repo, &lock_path, Some(&scans), None);
    let UnlockInspection::Found(findings) = inspection else {
        panic!("expected Found");
    };

    assert_eq!(findings.verdict, UnlockVerdict::Confirm);
    assert!(lock_path.exists());
}

#[test]
fn given_holders_that_could_not_be_determined_when_inspected_then_verdict_is_confirm_and_unknown_is_reported()
{
    let (_root, paths) = identity("unlock-unknown");
    let lock_path = strand(&paths.repo, "index.lock");
    let unknown = UnlockScans {
        find_holders: Arc::new(|_| None),
        find_repo_users: Arc::new(|_| None),
    };

    let inspection = inspect_stranded_git_lock(&paths.repo, &lock_path, Some(&unknown), None);
    let UnlockInspection::Found(findings) = inspection else {
        panic!("expected Found");
    };

    assert_eq!(findings.verdict, UnlockVerdict::Confirm);
    assert_eq!(findings.holders, None);
    assert_eq!(findings.repo_users, None);
}

#[test]
fn given_two_stranded_locks_when_unlock_runs_on_one_confirmed_inspection_then_only_that_lock_is_removed()
{
    let (_root, paths) = identity("unlock-only-one");
    let index_lock = strand(&paths.repo, "index.lock");
    let ref_lock = strand(&paths.repo, "refs/heads/main.lock");
    let scans = nobody_scans();
    let UnlockInspection::Found(confirmed) =
        inspect_stranded_git_lock(&paths.repo, &index_lock, Some(&scans), None)
    else {
        panic!("expected Found");
    };

    let outcome =
        unlock_stranded_git_lock(&paths.repo, &confirmed, Some(&scans)).expect("unlock");

    assert!(matches!(outcome, UnlockOutcome::Removed { .. }));
    assert!(!index_lock.exists());
    assert!(ref_lock.exists());
}

#[test]
fn given_the_lock_was_replaced_after_the_confirmed_inspection_when_unlock_runs_then_it_refuses_as_changed_and_new_lock_stays()
{
    let (_root, paths) = identity("unlock-changed");
    let lock_path = strand(&paths.repo, "index.lock");
    let scans = nobody_scans();
    let UnlockInspection::Found(confirmed) =
        inspect_stranded_git_lock(&paths.repo, &lock_path, Some(&scans), None)
    else {
        panic!("expected Found");
    };
    std::fs::remove_file(&lock_path).expect("remove lock");
    std::fs::write(&lock_path, "new writer").expect("write new lock");

    let outcome =
        unlock_stranded_git_lock(&paths.repo, &confirmed, Some(&scans)).expect("unlock");

    assert!(matches!(outcome, UnlockOutcome::Changed { .. }));
    assert!(lock_path.exists());
}
