use std::path::Path;

use isolation_core::backends::git_fixture::{git, repo};
use isolation_core::test_support::fixture;
use isolation_core::{
    capture_baseline, capture_delta_patch, cleanup_isolation, ensure_isolation, merge_isolated_changes,
    BackendRef, EnsureIsolationOptions, IsolationBackend, IsolationContext, IsolationMergeOptions,
    MergeMode, RcopyBackend, ISOLATION_BASELINE_MAX_CONTENT_BYTES,
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

fn nested_repo_at(path: &Path) {
    std::fs::create_dir_all(path).expect("nested path");
    git(path, &["init"]).expect("init");
    git(path, &["config", "user.name", "Fixture"]).expect("name");
    git(path, &["config", "user.email", "fixture@example.invalid"]).expect("email");
    // Match the shared repo() fixture: a system-wide autocrlf=true (the Windows
    // runner default) would rewrite every applied patch into CRLF and break the
    // byte-exact round-trip assertions below.
    git(path, &["config", "core.autocrlf", "false"]).expect("autocrlf");
    std::fs::write(path.join("inner-file"), "inner\n").expect("inner file");
    git(path, &["add", "."]).expect("add");
    git(path, &["commit", "-m", "inner"]).expect("commit");
}

fn context(base_dir: &Path) -> IsolationContext {
    IsolationContext {
        id: "seed".to_string(),
        base_dir: base_dir.to_path_buf(),
        cross_device: false,
        max_copy_bytes: None,
    }
}

#[test]
fn rcopy_seeds_an_untracked_embedded_repository_including_its_git_metadata() {
    let f = repo();
    nested_repo_at(&f.repo_root.join("vendor/inner"));
    let base_dir = f.root.join("base");
    let merged = base_dir.join("m");
    RcopyBackend
        .start(&f.repo_root, &merged, &context(&base_dir))
        .expect("start");
    assert_eq!(
        std::fs::read_to_string(merged.join("vendor/inner/inner-file")).expect("inner"),
        "inner\n"
    );
    assert!(std::fs::read_to_string(merged.join("vendor/inner/.git/HEAD"))
        .expect("HEAD")
        .contains("ref:"));
}

#[test]
fn delta_capture_refuses_a_baseline_nested_repository_missing_from_the_isolation() {
    let f = repo();
    nested_repo_at(&f.repo_root.join("vendor/inner"));
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    assert!(baseline
        .nested
        .iter()
        .any(|nested| nested.relative_path == "vendor/inner"));
    let isolation_dir = f.root.join("child");
    copy_dir_all(&f.repo_root, &isolation_dir).expect("copy");
    // The state the unseeded rcopy used to publish: the nested repository never arrived.
    std::fs::remove_dir_all(isolation_dir.join("vendor/inner")).expect("remove");
    let error = capture_delta_patch(&isolation_dir, &baseline).expect_err("must fail");
    assert!(error.to_string().contains("missing"));
}

#[test]
fn rcopy_isolation_round_trips_edits_to_an_untracked_embedded_repository() {
    let f = repo();
    nested_repo_at(&f.repo_root.join("vendor/inner"));
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let handle = ensure_isolation(EnsureIsolationOptions {
        repo_root: f.repo_root.clone(),
        id: "roundtrip".to_string(),
        preferred: Some(isolation_core::BackendKind::Rcopy),
        backends: vec![std::sync::Arc::new(RcopyBackend)],
        platform: None,
        home_dir: Some(f.home_dir.clone()),
        owner: None,
        max_copy_bytes: None,
    })
    .expect("handle");
    std::fs::write(handle.merged_dir.join("vendor/inner/inner-file"), "edited\n").expect("edit");
    let result = merge_isolated_changes(IsolationMergeOptions {
        id: "roundtrip".to_string(),
        artifacts_dir: f.root.join("artifacts"),
        lock_hook: None,
        commit_message: None,
        mode: MergeMode::Patch,
        apply: true,
        repo_root: f.repo_root.clone(),
        isolation_dir: handle.merged_dir.clone(),
        baseline,
        delta: None,
    })
    .expect("merge");
    assert!(result.state.nested_failed.unwrap_or_default().is_empty());
    assert_eq!(
        git(
            &f.repo_root.join("vendor/inner"),
            &["log", "-1", "--format=%s"]
        )
        .expect("log"),
        "chore(task): isolated nested changes"
    );
    assert_eq!(
        std::fs::read_to_string(f.repo_root.join("vendor/inner/inner-file")).expect("inner"),
        "edited\n"
    );
    cleanup_isolation(&handle).expect("cleanup");
}

#[test]
fn rcopy_teardown_fails_closed_when_worktree_removal_fails_for_a_live_registration() {
    let f = repo();
    let merged = f.root.join("merged");
    git(
        &f.repo_root,
        &["worktree", "add", "--detach", &merged.to_string_lossy(), "HEAD"],
    )
    .expect("worktree");
    // Corrupt the registration back-pointer: removal fails while the admin directory is live.
    std::fs::remove_file(f.repo_root.join(".git/worktrees/merged/gitdir")).expect("remove gitdir");
    let error = RcopyBackend.stop(&merged).expect_err("must fail");
    assert!(error.to_string().contains("worktree remove"));
    assert!(merged.exists());
}

#[test]
fn rcopy_teardown_proceeds_when_the_registration_is_already_gone() {
    let f = repo();
    let merged = f.root.join("merged");
    git(
        &f.repo_root,
        &["worktree", "add", "--detach", &merged.to_string_lossy(), "HEAD"],
    )
    .expect("worktree");
    std::fs::remove_dir_all(f.repo_root.join(".git/worktrees/merged")).expect("remove");
    RcopyBackend.stop(&merged).expect("stop");
    assert!(!merged.exists());
}

#[test]
fn rcopy_plain_copy_does_not_descend_into_its_own_destination() {
    let f = fixture();
    let lower = f.root.join("plain");
    std::fs::create_dir_all(lower.join("nested")).expect("nested");
    std::fs::write(lower.join("nested/file"), "data").expect("file");
    // Pathological layout (defended in depth): the destination lives inside the
    // source, as it did when a subvolume repository root stopped the device walk.
    let base_dir = lower.join(".omo-wt/tselfcopy");
    let merged = base_dir.join("m");
    std::fs::create_dir_all(&base_dir).expect("base");
    RcopyBackend
        .start(&lower, &merged, &context(&base_dir))
        .expect("start");
    assert_eq!(
        std::fs::read_to_string(merged.join("nested/file")).expect("file"),
        "data"
    );
    assert!(!merged.join(".omo-wt/tselfcopy/m").exists());
    let _: BackendRef = std::sync::Arc::new(RcopyBackend);
}
