use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use isolation_core::backends::git_fixture::{git, repo};
use isolation_core::test_support::Fixture;
use isolation_core::{
    capture_baseline, capture_delta_patch, commit_to_branch, merge_isolated_changes,
    merge_task_branch, IsolationError, IsolationMergeOptions, LockEvent, MergeKind, MergeMode,
    ISOLATION_BASELINE_MAX_CONTENT_BYTES,
};

fn copy_dir_all(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

struct Setup {
    f: Fixture,
    isolation_dir: PathBuf,
    artifacts_dir: PathBuf,
}

fn setup(nested: bool) -> Setup {
    let f = repo();
    if nested {
        let inner = repo();
        copy_dir_all(&inner.repo_root, &f.repo_root.join("inner")).expect("nested");
    }
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let isolation_dir = f.root.join("child");
    copy_dir_all(&f.repo_root, &isolation_dir).expect("isolation copy");
    let artifacts_dir = f.root.join("artifacts");
    let _ = baseline;
    Setup {
        f,
        isolation_dir,
        artifacts_dir,
    }
}

fn options(setup: &Setup, mode: MergeMode) -> IsolationMergeOptions {
    let baseline = capture_baseline(&setup.f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES)
        .expect("baseline");
    IsolationMergeOptions {
        id: "test".to_string(),
        artifacts_dir: setup.artifacts_dir.clone(),
        lock_hook: None,
        commit_message: None,
        mode,
        apply: true,
        repo_root: setup.f.repo_root.clone(),
        isolation_dir: setup.isolation_dir.clone(),
        baseline,
        delta: None,
    }
}

fn commit(dir: &Path, name: &str, text: &str) {
    std::fs::write(dir.join(name), text).expect("write");
    git(dir, &["add", name]).expect("add");
    git(dir, &["commit", "-m", name]).expect("commit");
}

fn text(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).expect("read")
}

#[test]
fn patch_applies_child_changes_and_writes_readable_artifacts_and_summary() {
    let s = setup(false);
    std::fs::write(s.isolation_dir.join("tracked"), "child\n").expect("edit");
    let result = merge_isolated_changes(options(&s, MergeMode::Patch)).expect("merge");
    assert_eq!(result.state.kind, MergeKind::Applied);
    assert!(result.state.changes_applied);
    assert_eq!(result.files_changed, 1);
    assert_eq!(text(&s.f.repo_root, "tracked"), "child\n");
    assert_eq!(
        git(&s.f.repo_root, &["status", "--porcelain"]).expect("status"),
        "M tracked"
    );
    let patch = result.patch_path.clone().expect("patch path");
    assert!(std::fs::read_to_string(&patch).expect("patch").contains("+child"));
    assert!(std::fs::read_to_string(&result.summary_path)
        .expect("summary")
        .contains("applied"));
}

#[test]
fn moved_context_fails_closed_atomically_retaining_patch_and_git_conflict() {
    let s = setup(false);
    std::fs::write(s.isolation_dir.join("tracked"), "child\n").expect("edit");
    std::fs::write(s.isolation_dir.join("new"), "must not land\n").expect("new");
    std::fs::write(s.f.repo_root.join("tracked"), "parent\n").expect("parent");
    let before = git(&s.f.repo_root, &["diff", "--binary"]).expect("diff");
    let result = merge_isolated_changes(options(&s, MergeMode::Patch)).expect("merge");
    assert_eq!(result.state.kind, MergeKind::NotApplied);
    assert!(!result.state.changes_applied);
    assert!(result
        .state
        .conflict
        .clone()
        .unwrap_or_default()
        .contains("patch does not apply"));
    assert_eq!(
        result.state.manual_command,
        Some(format!("git apply --3way {}", result.patch_path.clone().unwrap_or_default()))
    );
    assert_eq!(git(&s.f.repo_root, &["diff", "--binary"]).expect("diff"), before);
    assert_eq!(
        git(&s.f.repo_root, &["ls-files", "--others", "--exclude-standard"]).expect("untracked"),
        ""
    );
    assert!(std::fs::read_to_string(result.patch_path.expect("patch"))
        .expect("patch")
        .contains("must not land"));
}

#[test]
fn same_delta_twice_is_already_applied() {
    let s = setup(false);
    std::fs::write(s.isolation_dir.join("tracked"), "child\n").expect("edit");
    let delta = capture_delta_patch(
        &s.isolation_dir,
        &capture_baseline(&s.f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline"),
    )
    .expect("delta");
    let first = merge_isolated_changes(IsolationMergeOptions {
        delta: Some(delta.clone()),
        ..options(&s, MergeMode::Patch)
    })
    .expect("first");
    assert_eq!(first.state.kind, MergeKind::Applied);
    let second = merge_isolated_changes(IsolationMergeOptions {
        delta: Some(delta),
        ..options(&s, MergeMode::Patch)
    })
    .expect("second");
    assert_eq!(second.state.kind, MergeKind::AlreadyApplied);
    assert!(second.state.changes_applied);
    assert_eq!(text(&s.f.repo_root, "tracked"), "child\n");
}

#[test]
fn apply_false_retains_artifacts_without_parent_changes_or_task_branch() {
    let s = setup(false);
    commit(&s.isolation_dir, "new", "new\n");
    for mode in [MergeMode::Patch, MergeMode::Branch] {
        let result = merge_isolated_changes(IsolationMergeOptions {
            apply: false,
            ..options(&s, mode)
        })
        .expect("merge");
        assert_eq!(result.state.kind, MergeKind::Retained);
        assert!(!result.state.changes_applied);
        assert!(std::fs::read_to_string(result.patch_path.expect("patch"))
            .expect("patch")
            .contains("+new"));
    }
    assert_eq!(git(&s.f.repo_root, &["status", "--porcelain"]).expect("status"), "");
    assert_eq!(
        git(&s.f.repo_root, &["branch", "--list", "omo/task/*"]).expect("branches"),
        ""
    );
}

#[test]
fn empty_delta_writes_no_changes_summary() {
    let s = setup(false);
    let result = merge_isolated_changes(options(&s, MergeMode::Patch)).expect("merge");
    assert_eq!(result.state.kind, MergeKind::NoChanges);
    assert_eq!(result.files_changed, 0);
    assert!(std::fs::read_to_string(&result.summary_path)
        .expect("summary")
        .contains("no-changes"));
}

#[test]
fn clean_branch_preserves_two_child_commits_merges_onto_parent_head_and_deletes_branch() {
    let s = setup(false);
    commit(&s.isolation_dir, "first", "first\n");
    commit(&s.isolation_dir, "second", "second\n");
    let baseline = capture_baseline(&s.f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let branch = commit_to_branch(
        &s.isolation_dir,
        &s.f.repo_root,
        "test",
        &baseline,
        &isolation_core::BranchOptions::default(),
    )
    .expect("branch");
    assert_eq!(branch.branch_name.as_deref(), Some("omo/task/test"));
    assert_eq!(
        git(
            &s.f.repo_root,
            &[
                "rev-list",
                "--count",
                &format!("{}..{}", branch.base_sha, branch.branch_name.clone().unwrap_or_default())
            ]
        )
        .expect("count"),
        "2"
    );
    commit(&s.f.repo_root, "parent", "parent\n");
    let state = merge_task_branch(&s.f.repo_root, &branch, None).expect("merge");
    assert_eq!(state.kind, MergeKind::BranchMerged);
    assert_eq!(
        git(&s.f.repo_root, &["log", "-3", "--format=%s"]).expect("log"),
        "second\nfirst\nparent"
    );
    assert_eq!(
        git(
            &s.f.repo_root,
            &["branch", "--list", branch.branch_name.as_deref().unwrap_or_default()]
        )
        .expect("branches"),
        ""
    );
}

#[test]
fn branch_trailing_leftovers_use_callback_without_changing_child_or_parent_wip() {
    let s = setup(false);
    commit(&s.isolation_dir, "first", "first\n");
    std::fs::write(s.isolation_dir.join("leftover"), "left\n").expect("leftover");
    let callback: CommitMessage = Arc::new(|_patch| Ok(Some("generated".to_string())));
    let result = merge_isolated_changes(IsolationMergeOptions {
        commit_message: Some(callback),
        ..options(&s, MergeMode::Branch)
    })
    .expect("merge");
    assert_eq!(result.state.kind, MergeKind::BranchMerged);
    assert_eq!(
        git(&s.f.repo_root, &["log", "-2", "--format=%s"]).expect("log"),
        "generated\nfirst"
    );
    assert_eq!(
        git(&s.isolation_dir, &["status", "--porcelain"]).expect("status"),
        "?? leftover"
    );
}

#[test]
fn branch_stashes_parent_wip_and_restores_byte_identically() {
    let s = setup(false);
    commit(&s.isolation_dir, "new", "new\n");
    std::fs::write(s.f.repo_root.join("tracked"), "staged\n").expect("staged");
    git(&s.f.repo_root, &["add", "tracked"]).expect("add");
    std::fs::write(s.f.repo_root.join("tracked"), "unstaged\n").expect("unstaged");
    std::fs::write(s.f.repo_root.join("untracked"), [0u8, 255, 2]).expect("untracked");
    let staged = git(&s.f.repo_root, &["diff", "--cached", "--binary"]).expect("staged");
    let unstaged = git(&s.f.repo_root, &["diff", "--binary"]).expect("unstaged");
    let result = merge_isolated_changes(options(&s, MergeMode::Branch)).expect("merge");
    assert_eq!(result.state.kind, MergeKind::BranchMerged);
    assert_eq!(git(&s.f.repo_root, &["diff", "--cached", "--binary"]).expect("staged"), staged);
    assert_eq!(git(&s.f.repo_root, &["diff", "--binary"]).expect("unstaged"), unstaged);
    assert_eq!(
        std::fs::read(s.f.repo_root.join("untracked")).expect("untracked"),
        vec![0u8, 255, 2]
    );
    assert_eq!(git(&s.f.repo_root, &["stash", "list"]).expect("stash"), "");
}

#[test]
fn branch_conflict_aborts_entire_range_and_keeps_recoverable_branch() {
    let s = setup(false);
    commit(&s.isolation_dir, "first", "first\n");
    commit(&s.isolation_dir, "tracked", "child\n");
    commit(&s.f.repo_root, "tracked", "parent\n");
    let head = git(&s.f.repo_root, &["rev-parse", "HEAD"]).expect("head");
    let result = merge_isolated_changes(options(&s, MergeMode::Branch)).expect("merge");
    assert_eq!(result.state.kind, MergeKind::BranchMergeFailed);
    assert!(!result.state.changes_applied);
    assert!(result.state.conflict.is_some());
    assert_eq!(result.state.branch_name.as_deref(), Some("omo/task/test"));
    assert_eq!(git(&s.f.repo_root, &["rev-parse", "HEAD"]).expect("head"), head);
    assert_eq!(git(&s.f.repo_root, &["status", "--porcelain"]).expect("status"), "");
    assert!(git(
        &s.f.repo_root,
        &["branch", "--list", result.state.branch_name.as_deref().unwrap_or_default()]
    )
    .expect("branches")
    .contains("omo/task/test"));
}

#[test]
fn stash_restore_collision_reports_warning_and_never_drops_users_stash() {
    let s = setup(false);
    commit(&s.isolation_dir, "tracked", "child\n");
    std::fs::write(s.f.repo_root.join("tracked"), "parent WIP\n").expect("parent WIP");
    let result = merge_isolated_changes(options(&s, MergeMode::Branch)).expect("merge");
    assert_eq!(result.state.kind, MergeKind::BranchMerged);
    assert!(result.state.changes_applied);
    assert!(result
        .state
        .warning
        .clone()
        .unwrap_or_default()
        .contains("stash"));
    assert!(git(&s.f.repo_root, &["stash", "list"])
        .expect("stash list")
        .contains("omo-task-merge"));
    assert_eq!(
        git(&s.f.repo_root, &["show", "stash@{0}:tracked"]).expect("stash show"),
        "parent WIP"
    );
}

#[test]
fn nested_patches_land_after_root_and_preserve_staged_nested_wip() {
    let s = setup(true);
    std::fs::write(s.isolation_dir.join("tracked"), "root\n").expect("root");
    std::fs::write(s.isolation_dir.join("inner/tracked"), "nested\n").expect("nested");
    std::fs::write(s.f.repo_root.join("inner/wip"), "user\n").expect("wip");
    git(&s.f.repo_root.join("inner"), &["add", "wip"]).expect("add");
    let result = merge_isolated_changes(options(&s, MergeMode::Patch)).expect("merge");
    assert_eq!(result.state.kind, MergeKind::Applied);
    assert!(result.state.partial.is_none());
    assert_eq!(text(&s.f.repo_root.join("inner"), "tracked"), "nested\n");
    assert_eq!(
        git(&s.f.repo_root.join("inner"), &["status", "--porcelain"]).expect("status"),
        "A  wip"
    );
    assert_eq!(
        git(&s.f.repo_root.join("inner"), &["stash", "list"]).expect("stash"),
        ""
    );
    assert_eq!(
        result.nested_patch_paths.as_ref().map(|paths| paths.len()),
        Some(1)
    );
}

#[test]
fn root_failure_leaves_nested_repo_untouched() {
    let s = setup(true);
    std::fs::write(s.isolation_dir.join("tracked"), "child\n").expect("child");
    std::fs::write(s.isolation_dir.join("inner/tracked"), "nested\n").expect("nested");
    std::fs::write(s.f.repo_root.join("tracked"), "parent\n").expect("parent");
    let result = merge_isolated_changes(options(&s, MergeMode::Patch)).expect("merge");
    assert_eq!(result.state.kind, MergeKind::NotApplied);
    assert_eq!(text(&s.f.repo_root.join("inner"), "tracked"), "base\n");
    assert_eq!(
        git(&s.f.repo_root.join("inner"), &["status", "--porcelain"]).expect("status"),
        ""
    );
}

#[test]
fn nested_failure_keeps_successful_root_and_identifies_partial_result() {
    let s = setup(true);
    std::fs::write(s.isolation_dir.join("tracked"), "root\n").expect("root");
    std::fs::write(s.isolation_dir.join("inner/tracked"), "nested\n").expect("nested");
    commit(&s.f.repo_root.join("inner"), "tracked", "conflicting\n");
    let result = merge_isolated_changes(options(&s, MergeMode::Patch)).expect("merge");
    assert_eq!(result.state.kind, MergeKind::Applied);
    assert!(result.state.changes_applied);
    assert_eq!(result.state.partial, Some(true));
    let failed = result.state.nested_failed.clone().unwrap_or_default();
    assert_eq!(failed[0].path, "inner");
    assert!(failed[0].error.contains("patch does not apply"));
    assert_eq!(text(&s.f.repo_root, "tracked"), "root\n");
    assert_eq!(text(&s.f.repo_root.join("inner"), "tracked"), "conflicting\n");
}

#[test]
fn linked_parent_uses_common_git_directory_lock() {
    let s = setup(false);
    let linked = s.f.root.join("linked");
    git(
        &s.f.repo_root,
        &["worktree", "add", "-b", "linked", &linked.to_string_lossy()],
    )
    .expect("worktree");
    commit(&s.isolation_dir, "new", "new\n");
    let lock_path = Arc::new(Mutex::new(PathBuf::new()));
    let sink = Arc::clone(&lock_path);
    let result = merge_isolated_changes(IsolationMergeOptions {
        repo_root: linked.clone(),
        lock_hook: Some(Arc::new(move |event, path| {
            if event == LockEvent::Acquired {
                assert!(path.is_file());
                *sink.lock().unwrap_or_else(|poison| poison.into_inner()) = path.to_path_buf();
            }
        })),
        ..options(&s, MergeMode::Branch)
    })
    .expect("merge");
    assert_eq!(result.state.kind, MergeKind::BranchMerged);
    let common = git(&linked, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .expect("common");
    assert_eq!(
        *lock_path.lock().unwrap_or_else(|poison| poison.into_inner()),
        PathBuf::from(common).join("omo-isolation-merge.lock")
    );
    assert_eq!(text(&linked, "new"), "new\n");
}

fn event_name(event: LockEvent) -> &'static str {
    match event {
        LockEvent::Waiting => "waiting",
        LockEvent::Acquired => "acquired",
        LockEvent::Released => "released",
    }
}

fn wait_for(gate: &(Mutex<bool>, Condvar), expected: bool) {
    let (flag, condvar) = gate;
    let mut value = flag.lock().unwrap_or_else(|poison| poison.into_inner());
    while *value != expected {
        value = condvar
            .wait(value)
            .unwrap_or_else(|poison| poison.into_inner());
    }
}

fn set_gate(gate: &(Mutex<bool>, Condvar), value: bool) {
    let (flag, condvar) = gate;
    *flag.lock().unwrap_or_else(|poison| poison.into_inner()) = value;
    condvar.notify_all();
}

fn concurrent(mode: MergeMode) {
    let s = setup(false);
    std::fs::write(s.isolation_dir.join("one"), "one\n").expect("one");
    let other = s.f.root.join("other");
    copy_dir_all(&s.f.repo_root, &other).expect("copy");
    std::fs::write(other.join("two"), "two\n").expect("two");
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let first_entered = Arc::new((Mutex::new(false), Condvar::new()));
    let release_first = Arc::new((Mutex::new(false), Condvar::new()));
    let second_queued = Arc::new((Mutex::new(false), Condvar::new()));
    let first_options = IsolationMergeOptions {
        lock_hook: Some(Arc::new({
            let events = Arc::clone(&events);
            let entered = Arc::clone(&first_entered);
            let release = Arc::clone(&release_first);
            move |event, _path| {
                events
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(format!("first:{}", event_name(event)));
                if event == LockEvent::Acquired {
                    set_gate(&entered, true);
                    wait_for(&release, true);
                }
            }
        })),
        ..options(&s, mode)
    };
    let first = std::thread::spawn(move || merge_isolated_changes(first_options));
    wait_for(&first_entered, true);
    let second_options = IsolationMergeOptions {
        id: "second".to_string(),
        isolation_dir: other,
        lock_hook: Some(Arc::new({
            let events = Arc::clone(&events);
            let queued = Arc::clone(&second_queued);
            move |event, _path| {
                events
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .push(format!("second:{}", event_name(event)));
                if event == LockEvent::Waiting {
                    set_gate(&queued, true);
                }
            }
        })),
        ..options(&s, mode)
    };
    let second = std::thread::spawn(move || merge_isolated_changes(second_options));
    wait_for(&second_queued, true);
    set_gate(&release_first, true);
    let expected_kind = if mode == MergeMode::Patch {
        MergeKind::Applied
    } else {
        MergeKind::BranchMerged
    };
    for joined in [first.join(), second.join()] {
        let result = joined.expect("thread").expect("merge");
        assert_eq!(result.state.kind, expected_kind);
    }
    assert_eq!(
        *events.lock().unwrap_or_else(|poison| poison.into_inner()),
        vec![
            "first:waiting",
            "first:acquired",
            "second:waiting",
            "first:released",
            "second:acquired",
            "second:released",
        ]
    );
    assert_eq!(text(&s.f.repo_root, "one"), "one\n");
    assert_eq!(text(&s.f.repo_root, "two"), "two\n");
}

#[test]
fn concurrent_patch_merges_serialize_acquire_release() {
    concurrent(MergeMode::Patch);
}

#[test]
fn concurrent_branch_merges_serialize_acquire_release() {
    concurrent(MergeMode::Branch);
}

#[test]
fn dirty_baseline_filters_user_wip_out_of_each_child_commit() {
    let s = setup(false);
    std::fs::write(s.f.repo_root.join("tracked"), "user WIP\n").expect("wip");
    std::fs::write(s.f.repo_root.join("user-file"), "user untracked\n").expect("untracked");
    let baseline = capture_baseline(&s.f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let isolation_dir = s.f.root.join("dirty-child");
    copy_dir_all(&s.f.repo_root, &isolation_dir).expect("copy");
    git(&isolation_dir, &["add", "."]).expect("add");
    commit(&isolation_dir, "first", "first\n");
    commit(&isolation_dir, "second", "second\n");
    let result = merge_isolated_changes(IsolationMergeOptions {
        baseline,
        isolation_dir,
        ..options(&s, MergeMode::Branch)
    })
    .expect("merge");
    assert_eq!(result.state.kind, MergeKind::BranchMerged);
    assert_eq!(
        git(&s.f.repo_root, &["log", "-2", "--format=%s"]).expect("log"),
        "second\nfirst"
    );
    assert_eq!(
        git(&s.f.repo_root, &["show", "HEAD:tracked"]).expect("show"),
        "base"
    );
    assert!(!git(&s.f.repo_root, &["ls-tree", "--name-only", "HEAD"])
        .expect("tree")
        .contains("user-file"));
    assert_eq!(text(&s.f.repo_root, "tracked"), "user WIP\n");
    assert_eq!(text(&s.f.repo_root, "user-file"), "user untracked\n");
}

#[test]
fn dirty_baseline_overlapping_commit_fails_with_typed_commit_identity_and_retained_branch() {
    let s = setup(false);
    std::fs::write(s.f.repo_root.join("tracked"), "user WIP\n").expect("wip");
    let baseline = capture_baseline(&s.f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let isolation_dir = s.f.root.join("dirty-child");
    copy_dir_all(&s.f.repo_root, &isolation_dir).expect("copy");
    commit(&isolation_dir, "tracked", "overlapping child\n");
    let sha = git(&isolation_dir, &["rev-parse", "HEAD"]).expect("head");
    let error = commit_to_branch(
        &isolation_dir,
        &s.f.repo_root,
        "test",
        &baseline,
        &isolation_core::BranchOptions::default(),
    )
    .expect_err("must fail");
    match error {
        IsolationError::CommitReplay { commit, .. } => assert_eq!(commit, sha),
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(text(&s.f.repo_root, "tracked"), "user WIP\n");
    assert!(git(&s.f.repo_root, &["branch", "--list", "omo/task/test"])
        .expect("branches")
        .contains("omo/task/test"));
}

#[test]
fn malformed_patch_reports_not_applied_without_touching_parent() {
    let s = setup(false);
    let result = merge_isolated_changes(IsolationMergeOptions {
        delta: Some(isolation_core::DeltaPatchResult {
            root_patch: "invalid patch\n".to_string(),
            nested_patches: Vec::new(),
        }),
        ..options(&s, MergeMode::Patch)
    })
    .expect("merge");
    assert_eq!(result.state.kind, MergeKind::NotApplied);
    assert!(!result.state.changes_applied);
    assert!(result.state.conflict.is_some());
    assert_eq!(git(&s.f.repo_root, &["status", "--porcelain"]).expect("status"), "");
}

#[test]
fn same_root_and_nested_delta_twice_does_not_create_spurious_nested_failure() {
    let s = setup(true);
    std::fs::write(s.isolation_dir.join("tracked"), "root\n").expect("root");
    std::fs::write(s.isolation_dir.join("inner/tracked"), "nested\n").expect("nested");
    let baseline = capture_baseline(&s.f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let delta = capture_delta_patch(&s.isolation_dir, &baseline).expect("delta");
    let first = merge_isolated_changes(IsolationMergeOptions {
        delta: Some(delta.clone()),
        baseline: baseline.clone(),
        ..options(&s, MergeMode::Patch)
    })
    .expect("first");
    assert_eq!(first.state.kind, MergeKind::Applied);
    let head = git(&s.f.repo_root.join("inner"), &["rev-parse", "HEAD"]).expect("head");
    let second = merge_isolated_changes(IsolationMergeOptions {
        delta: Some(delta),
        baseline,
        ..options(&s, MergeMode::Patch)
    })
    .expect("second");
    assert_eq!(second.state.kind, MergeKind::AlreadyApplied);
    assert!(second.state.partial.is_none());
    assert_eq!(
        git(&s.f.repo_root.join("inner"), &["rev-parse", "HEAD"]).expect("head"),
        head
    );
}
