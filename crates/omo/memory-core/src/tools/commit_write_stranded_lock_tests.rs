use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::git::{
    GitCommitAuthor, GitExec, GitExecOptions, GitExecResult, GitMemoryRepo, GitMemoryRepoOptions,
    UnlockInspection, UnlockOutcome, UnlockScans, UnlockVerdict, inspect_stranded_git_lock,
    restore_worktree_from_head, system_git_exec, unlock_stranded_git_lock,
};
use crate::memfs::{MemoryFrontmatter, render_memory_file};
use crate::tools::{
    MemoryApplyPatchParams, MemoryToolParams, run_memory_apply_patch, run_memory_tool,
};

fn author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "agent-stranded-lock-write".to_string(),
        author_name: "Stranded Lock Write".to_string(),
        author_email: None,
    }
}

fn nobody_scans() -> UnlockScans {
    UnlockScans {
        find_holders: Arc::new(|_| Some(Vec::new())),
        find_repo_users: Arc::new(|_| Some(Vec::new())),
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    repo: GitMemoryRepo,
    lock_path: PathBuf,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().expect("tempdir");
    let canonical = std::fs::canonicalize(root.path()).unwrap_or_else(|_| root.path().to_path_buf());
    let repo_dir = canonical.join("repo");
    let auth = author();
    let repo = GitMemoryRepo::open(&repo_dir, &auth.agent_id).expect("open");
    repo.init(None).expect("init");
    let lock_path = repo.dir.join(".git").join("index.lock");
    Fixture {
        _root: root,
        repo,
        lock_path,
    }
}

fn seed_file(repo: &GitMemoryRepo, path: &str, text: &str) {
    let full = repo.dir.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    let fm = MemoryFrontmatter {
        description: "Seed".to_string(),
        read_only: None,
        kind: None,
        aliases: None,
    };
    let rendered = render_memory_file(&fm, text).expect("render");
    std::fs::write(&full, rendered).expect("write");
    repo.commit_write(&[path], &format!("seed {path}"), &author())
        .expect("commit seed");
}

fn create_block(
    repo: &GitMemoryRepo,
    path: &str,
    text: &str,
    reason: &str,
) -> Result<crate::tools::MemoryToolResult, crate::tools::MemoryToolError> {
    run_memory_tool(
        repo,
        &author(),
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: reason.to_string(),
            file_path: Some(path.to_string()),
            description: Some("Fact".to_string()),
            file_text: Some(text.to_string()),
            ..Default::default()
        },
        None,
    )
}

struct IndexLockAtCommitExec {
    inner: Arc<dyn GitExec>,
    repo_dir: PathBuf,
}

impl GitExec for IndexLockAtCommitExec {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        if argv.first().map(String::as_str) == Some("commit") {
            let _ = std::fs::write(self.repo_dir.join(".git").join("index.lock"), "");
        }
        self.inner.run(argv, options)
    }
}

fn index_lock_at_commit_repo(dir: &Path) -> GitMemoryRepo {
    let auth = author();
    GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: dir.to_path_buf(),
        agent_id: auth.agent_id,
        exec: Some(Arc::new(IndexLockAtCommitExec {
            inner: system_git_exec(),
            repo_dir: dir.to_path_buf(),
        })),
        install_hooks: None,
    })
    .expect("repo")
}

struct HeadUnreadableExec {
    inner: Arc<dyn GitExec>,
}

impl GitExec for HeadUnreadableExec {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        if argv.first().map(String::as_str) == Some("show")
            && argv.get(1).is_some_and(|arg| arg.starts_with("HEAD:"))
        {
            return Ok(GitExecResult {
                code: 128,
                stdout: String::new(),
                stderr: "HEAD is unreadable in this test".to_string(),
            });
        }
        self.inner.run(argv, options)
    }
}

fn head_unreadable_repo(dir: &Path) -> GitMemoryRepo {
    let auth = author();
    GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: dir.to_path_buf(),
        agent_id: auth.agent_id,
        exec: Some(Arc::new(HeadUnreadableExec {
            inner: system_git_exec(),
        })),
        install_hooks: None,
    })
    .expect("repo")
}

#[test]
fn given_a_kept_index_lock_when_a_create_fails_then_new_file_is_gone_and_repo_is_clean_apart_from_lock()
{
    let setup = fixture();
    std::fs::write(&setup.lock_path, "").expect("write index.lock");

    let failure =
        create_block(&setup.repo, "system/new.md", "new fact", "write behind the lock")
            .expect_err("create fails");

    assert!(failure.message.contains("/memory unlock"));
    assert!(!setup.repo.dir.join("system").join("new.md").exists());
    assert_eq!(setup.repo.status(&[] as &[&str]).expect("status"), "");
    assert!(setup.lock_path.exists());
}

#[test]
fn given_a_kept_index_lock_when_str_replace_on_committed_file_fails_then_file_is_back_to_head_content()
{
    let setup = fixture();
    seed_file(&setup.repo, "system/facts.md", "old fact\n");
    let at_head =
        std::fs::read_to_string(setup.repo.dir.join("system/facts.md")).expect("read head");
    std::fs::write(&setup.lock_path, "").expect("write index.lock");

    let _ = run_memory_tool(
        &setup.repo,
        &author(),
        &MemoryToolParams {
            command: Some("str_replace".to_string()),
            reason: "edit behind the lock".to_string(),
            file_path: Some("system/facts.md".to_string()),
            old_string: Some("old fact".to_string()),
            new_string: Some("new fact".to_string()),
            ..Default::default()
        },
        None,
    );

    assert_eq!(
        std::fs::read_to_string(setup.repo.dir.join("system/facts.md")).expect("read after"),
        at_head
    );
    assert_eq!(setup.repo.status(&[] as &[&str]).expect("status"), "");
}

#[test]
fn given_a_write_failed_behind_a_kept_lock_when_unlocked_then_same_write_retried_commits() {
    let setup = fixture();
    std::fs::write(&setup.lock_path, "").expect("write index.lock");
    let _ = create_block(&setup.repo, "system/new.md", "new fact", "write behind the lock");

    let scans = nobody_scans();
    let UnlockInspection::Found(inspection) =
        inspect_stranded_git_lock(&setup.repo.dir, &setup.lock_path, Some(&scans), None)
    else {
        panic!("expected Found");
    };
    assert_eq!(inspection.verdict, UnlockVerdict::Confirm);
    let outcome =
        unlock_stranded_git_lock(&setup.repo.dir, &inspection, Some(&scans)).expect("unlock");
    assert!(matches!(outcome, UnlockOutcome::Removed { .. }));

    let retried = create_block(&setup.repo, "system/new.md", "new fact", "write after unlock")
        .expect("retried create");
    assert_eq!(
        retried.commit.expect("commit").affected_paths,
        vec!["system/new.md".to_string()]
    );
    assert_eq!(setup.repo.status(&[] as &[&str]).expect("status"), "");
}

#[test]
fn given_a_kept_ref_lock_that_lets_git_add_succeed_when_create_fails_at_commit_and_lock_is_unlocked_then_index_and_tree_are_clean_and_retry_commits()
{
    let setup = fixture();
    let ref_lock = setup.repo.dir.join(".git/refs/heads/main.lock");
    std::fs::write(&ref_lock, "").expect("write ref.lock");

    let failure =
        create_block(&setup.repo, "system/new.md", "new fact", "write behind the ref lock")
            .expect_err("create fails");
    let status_after_failure = setup.repo.status(&[] as &[&str]).expect("status");

    let scans = nobody_scans();
    let UnlockInspection::Found(inspection) =
        inspect_stranded_git_lock(&setup.repo.dir, &ref_lock, Some(&scans), None)
    else {
        panic!("expected Found");
    };
    let outcome =
        unlock_stranded_git_lock(&setup.repo.dir, &inspection, Some(&scans)).expect("unlock");
    assert!(matches!(outcome, UnlockOutcome::Removed { .. }));

    let retried = create_block(&setup.repo, "system/new.md", "new fact", "write after unlock")
        .expect("retried create");

    assert!(failure.message.contains("/memory unlock"));
    assert_eq!(status_after_failure, "");
    assert_eq!(
        retried.commit.expect("commit").affected_paths,
        vec!["system/new.md".to_string()]
    );
}

#[test]
fn given_index_lock_appears_after_git_add_when_commit_fails_and_lock_is_unlocked_then_next_write_unstages_first_and_commits()
{
    let setup = fixture();
    let locked_at_commit = index_lock_at_commit_repo(&setup.repo.dir);

    let failure = create_block(
        &locked_at_commit,
        "system/new.md",
        "new fact",
        "write behind a commit-time lock",
    )
    .expect_err("commit fails");

    let scans = nobody_scans();
    let UnlockInspection::Found(inspection) =
        inspect_stranded_git_lock(&setup.repo.dir, &setup.lock_path, Some(&scans), None)
    else {
        panic!("expected Found");
    };
    let outcome =
        unlock_stranded_git_lock(&setup.repo.dir, &inspection, Some(&scans)).expect("unlock");
    assert!(matches!(outcome, UnlockOutcome::Removed { .. }));

    let retried = create_block(&setup.repo, "system/other.md", "other fact", "write after unlock")
        .expect("retried create");

    assert!(failure.message.contains("/memory unlock"));
    assert!(failure.message.contains("still staged"));
    assert_eq!(
        retried.commit.expect("commit").affected_paths,
        vec!["system/other.md".to_string()]
    );
    assert_eq!(setup.repo.status(&[] as &[&str]).expect("status"), "");
    assert!(!setup.repo.dir.join("system/new.md").exists());
}

#[test]
fn given_the_restore_itself_cannot_read_head_when_write_fails_behind_lock_then_both_errors_are_reported_and_lock_is_kept()
{
    let setup = fixture();
    seed_file(&setup.repo, "system/facts.md", "old fact\n");
    let broken = head_unreadable_repo(&setup.repo.dir);
    std::fs::write(&setup.lock_path, "").expect("write index.lock");

    let failure = run_memory_tool(
        &broken,
        &author(),
        &MemoryToolParams {
            command: Some("str_replace".to_string()),
            reason: "edit behind the lock".to_string(),
            file_path: Some("system/facts.md".to_string()),
            old_string: Some("old fact".to_string()),
            new_string: Some("new fact".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect_err("fails");

    assert!(failure.message.contains("/memory unlock"));
    assert!(failure.message.contains("could not be undone"));
    assert!(setup.lock_path.exists());
}

#[test]
fn given_an_unrelated_dirty_file_when_only_the_writes_own_paths_are_restored_then_unrelated_change_survives()
{
    let setup = fixture();
    seed_file(&setup.repo, "system/facts.md", "old fact\n");
    seed_file(&setup.repo, "system/other.md", "other\n");
    std::fs::write(setup.repo.dir.join("system/facts.md"), "changed by the write")
        .expect("write facts");
    std::fs::write(setup.repo.dir.join("system/new.md"), "created by the write")
        .expect("write new");
    std::fs::write(
        setup.repo.dir.join("system/other.md"),
        "someone else's uncommitted work",
    )
    .expect("write other");

    restore_worktree_from_head(&setup.repo, &["system/facts.md", "system/new.md"])
        .expect("restore");

    assert!(
        std::fs::read_to_string(setup.repo.dir.join("system/facts.md"))
            .expect("read facts")
            .contains("old fact")
    );
    assert!(!setup.repo.dir.join("system/new.md").exists());
    assert_eq!(
        std::fs::read_to_string(setup.repo.dir.join("system/other.md")).expect("read other"),
        "someone else's uncommitted work"
    );
}

#[test]
fn given_a_pending_rollback_when_next_write_is_memory_apply_patch_then_it_is_settled_first_and_patch_commits()
{
    let setup = fixture();
    let locked_at_commit = index_lock_at_commit_repo(&setup.repo.dir);
    let failure = create_block(
        &locked_at_commit,
        "system/new.md",
        "new fact",
        "write behind a commit-time lock",
    )
    .expect_err("commit fails");
    assert!(failure.message.contains("still staged"));

    let scans = nobody_scans();
    let UnlockInspection::Found(inspection) =
        inspect_stranded_git_lock(&setup.repo.dir, &setup.lock_path, Some(&scans), None)
    else {
        panic!("expected Found");
    };
    let outcome =
        unlock_stranded_git_lock(&setup.repo.dir, &inspection, Some(&scans)).expect("unlock");
    assert!(matches!(outcome, UnlockOutcome::Removed { .. }));

    let result = run_memory_apply_patch(
        &setup.repo,
        &MemoryApplyPatchParams {
            reason: "patch after unlock".to_string(),
            author: author(),
            input: "*** Begin Patch\n*** Add File: system/other.md\n+---\n+description: Other\n+---\n+other fact\n*** End Patch".to_string(),
            provenance: None,
        },
        None,
    )
    .expect("patch after unlock");

    assert_eq!(
        result.commit.expect("commit").affected_paths,
        vec!["system/other.md".to_string()]
    );
    assert_eq!(setup.repo.status(&[] as &[&str]).expect("status"), "");
    assert!(!setup.repo.dir.join("system/new.md").exists());
}

#[test]
fn given_a_new_file_written_again_by_hand_after_staged_failure_when_next_write_settles_rollback_then_hand_written_file_survives_and_is_reported()
{
    let setup = fixture();
    let locked_at_commit = index_lock_at_commit_repo(&setup.repo.dir);
    let _ = create_block(
        &locked_at_commit,
        "system/new.md",
        "new fact",
        "write behind a commit-time lock",
    );
    let scans = nobody_scans();
    let UnlockInspection::Found(inspection) =
        inspect_stranded_git_lock(&setup.repo.dir, &setup.lock_path, Some(&scans), None)
    else {
        panic!("expected Found");
    };
    let _ = unlock_stranded_git_lock(&setup.repo.dir, &inspection, Some(&scans)).expect("unlock");

    let fm = MemoryFrontmatter {
        description: "Mine".to_string(),
        read_only: None,
        kind: None,
        aliases: None,
    };
    let hand_written = render_memory_file(&fm, "my own words\n").expect("render");
    std::fs::write(setup.repo.dir.join("system/new.md"), &hand_written).expect("hand write");

    let failure =
        create_block(&setup.repo, "system/other.md", "other fact", "next write").expect_err("kept");

    assert!(failure.message.contains("system/new.md"));
    assert!(failure.message.contains("changed after that failure"));
    assert_eq!(
        std::fs::read_to_string(setup.repo.dir.join("system/new.md")).expect("read"),
        hand_written
    );
    assert_eq!(
        setup.repo.status(&[] as &[&str]).expect("status"),
        "?? system/new.md\n"
    );
}
