use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use isolation_core::backends::git_fixture::{git, repo};
use isolation_core::{
    capture_baseline, capture_delta_patch, capture_repo_baseline, parse_diff_git_line_paths,
    run_git, str_args, BaselineCaptureOptions, BaselineReadRetryDetails, GitOptions,
    IsolationError, RepoBaseline, ISOLATION_BASELINE_MAX_CONTENT_BYTES,
};
use isolation_core::git::baseline::BaselineRunGit;

fn setup() -> isolation_core::test_support::Fixture {
    let f = repo();
    std::fs::write(f.repo_root.join("modify"), "before\n").expect("modify");
    std::fs::write(f.repo_root.join("delete"), "gone\n").expect("delete");
    git(&f.repo_root, &["add", "."]).expect("add");
    git(&f.repo_root, &["commit", "-m", "seed"]).expect("commit");
    f
}

fn child(source: &std::path::Path, root: &std::path::Path) -> std::path::PathBuf {
    let target = root.join("child");
    copy_dir_all(source, &target).expect("copy");
    target
}

fn copy_dir_all(source: &std::path::Path, destination: &std::path::Path) -> std::io::Result<()> {
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

fn paths(patch: &str) -> Vec<String> {
    let mut out: Vec<String> = patch
        .split('\n')
        .flat_map(parse_diff_git_line_paths)
        .collect();
    out.sort();
    out.dedup();
    out
}

#[test]
fn clean_baseline_delta_includes_tracked_additions_modifications_deletions_and_untracked_files() {
    let f = setup();
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let isolated = child(&f.repo_root, &f.root);
    std::fs::write(isolated.join("modify"), "after\n").expect("modify");
    std::fs::remove_file(isolated.join("delete")).expect("delete");
    std::fs::write(isolated.join("added"), "staged\n").expect("added");
    git(&isolated, &["add", "added"]).expect("add");
    std::fs::write(isolated.join("untracked"), "new\n").expect("untracked");
    let result = capture_delta_patch(&isolated, &baseline).expect("delta");
    assert_eq!(paths(&result.root_patch), vec!["added", "delete", "modify", "untracked"]);
    assert!(result.nested_patches.is_empty());
    assert_eq!(git(&f.repo_root, &["status", "--porcelain"]).expect("status"), "");
}

#[test]
fn parent_staged_unstaged_and_untracked_wip_is_excluded() {
    let f = setup();
    std::fs::write(f.repo_root.join("modify"), "staged\n").expect("staged");
    git(&f.repo_root, &["add", "modify"]).expect("add");
    std::fs::write(f.repo_root.join("modify"), "unstaged\n").expect("unstaged");
    std::fs::write(f.repo_root.join("parent-wip"), "parent\n").expect("parent-wip");
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    assert_eq!(baseline.root.untracked_files, vec!["parent-wip"]);
    let isolated = child(&f.repo_root, &f.root);
    std::fs::write(isolated.join("child-only"), "child\n").expect("child-only");
    let result = capture_delta_patch(&isolated, &baseline).expect("delta");
    assert_eq!(paths(&result.root_patch), vec!["child-only"]);
}

#[test]
fn two_child_commits_and_remaining_wip_are_represented_without_child_objects_in_parent() {
    let f = setup();
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let isolated = child(&f.repo_root, &f.root);
    for name in ["first", "second"] {
        std::fs::write(isolated.join(name), format!("{name}\n")).expect("commit file");
        git(&isolated, &["add", name]).expect("add");
        git(&isolated, &["commit", "-m", name]).expect("commit");
    }
    std::fs::write(isolated.join("wip"), "pending\n").expect("wip");
    let result = capture_delta_patch(&isolated, &baseline).expect("delta");
    assert_eq!(paths(&result.root_patch), vec!["first", "second", "wip"]);
}

#[test]
fn binary_untracked_patch_applies_byte_for_byte() {
    let f = setup();
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let isolated = child(&f.repo_root, &f.root);
    let bytes: Vec<u8> = vec![0, 255, 1, 0, 128, 42];
    std::fs::write(isolated.join("binary"), &bytes).expect("binary");
    let result = capture_delta_patch(&isolated, &baseline).expect("delta");
    assert!(result.root_patch.contains("GIT binary patch"));
    run_git(
        &str_args(&["apply", "--binary", "-"]),
        &GitOptions {
            cwd: f.repo_root.clone(),
            input: Some(result.root_patch.clone().into_bytes()),
            ..GitOptions::new(f.repo_root.clone())
        },
    )
    .expect("apply");
    assert_eq!(std::fs::read(f.repo_root.join("binary")).expect("binary"), bytes);
}

#[test]
fn spaces_quotes_and_unicode_paths_round_trip_through_patches_and_parsing() {
    let f = setup();
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let isolated = child(&f.repo_root, &f.root);
    let posix_only = !cfg!(windows);
    let mut names = vec!["space name".to_string()];
    if posix_only {
        names.push("quote\"name".to_string());
        names.push("tab\tname".to_string());
    }
    names.push("한글".to_string());
    for name in &names {
        std::fs::write(isolated.join(name), "new\n").expect("file");
    }
    let result = capture_delta_patch(&isolated, &baseline).expect("delta");
    let mut expected = names.clone();
    expected.sort();
    assert_eq!(paths(&result.root_patch), expected);
    run_git(
        &str_args(&["apply", "--binary", "-"]),
        &GitOptions {
            cwd: f.repo_root.clone(),
            input: Some(result.root_patch.clone().into_bytes()),
            ..GitOptions::new(f.repo_root.clone())
        },
    )
    .expect("apply");
    for name in &names {
        assert_eq!(
            std::fs::read_to_string(f.repo_root.join(name)).expect("file"),
            "new\n"
        );
    }
}

#[test]
fn nested_repository_delta_is_separate_and_node_modules_is_excluded() {
    let f = setup();
    let nested = repo();
    std::fs::create_dir_all(f.repo_root.join("libs")).expect("libs");
    copy_dir_all(&nested.repo_root, &f.repo_root.join("libs/inner")).expect("nested copy");
    std::fs::create_dir_all(f.repo_root.join("node_modules")).expect("node_modules");
    copy_dir_all(&nested.repo_root, &f.repo_root.join("node_modules/ignored"))
        .expect("ignored copy");
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let nested_paths: Vec<String> = baseline
        .nested
        .iter()
        .map(|nested| nested.relative_path.clone())
        .collect();
    assert_eq!(nested_paths, vec!["libs/inner"]);
    let isolated = child(&f.repo_root, &f.root);
    std::fs::write(isolated.join("libs/inner/new"), "nested\n").expect("nested new");
    let result = capture_delta_patch(&isolated, &baseline).expect("delta");
    assert_eq!(result.root_patch, "");
    assert_eq!(
        result
            .nested_patches
            .iter()
            .map(|nested| nested.relative_path.clone())
            .collect::<Vec<_>>(),
        vec!["libs/inner"]
    );
    assert_eq!(paths(&result.nested_patches[0].patch), vec!["new"]);
}

#[test]
fn a_timed_out_baseline_read_retries_once_with_a_fresh_process_and_completes() {
    let f = setup();
    std::fs::write(f.repo_root.join("untracked"), "new\n").expect("untracked");
    let index_before = std::fs::read(f.repo_root.join(".git/index")).expect("index");
    let attempts = Arc::new(AtomicUsize::new(0));
    let successful_args: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let successful_locks: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let retries: Arc<Mutex<Vec<BaselineReadRetryDetails>>> = Arc::new(Mutex::new(Vec::new()));
    let execute: BaselineRunGit = {
        let attempts = Arc::clone(&attempts);
        let successful_args = Arc::clone(&successful_args);
        let successful_locks = Arc::clone(&successful_locks);
        Arc::new(move |args, options| {
            if args.iter().any(|arg| arg == "ls-files") {
                let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
                if attempt == 1 {
                    return Err(IsolationError::GitTimeout {
                        args: args.to_vec(),
                        cwd: options.cwd.clone(),
                        timeout_ms: 50,
                        message: "git timed out after 50ms".to_string(),
                    });
                }
                *successful_args
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = args.to_vec();
                *successful_locks
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner()) = options
                    .env
                    .iter()
                    .find(|(key, _)| key == "GIT_OPTIONAL_LOCKS")
                    .map(|(_, value)| value.clone());
            }
            run_git(args, options)
        })
    };
    let baseline = capture_repo_baseline(
        &f.repo_root,
        ISOLATION_BASELINE_MAX_CONTENT_BYTES,
        &BaselineCaptureOptions {
            run_git: Some(execute),
            on_read_retry: Some(Arc::new({
                let retries = Arc::clone(&retries);
                move |details: &BaselineReadRetryDetails| {
                    retries
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .push(details.clone());
                }
            })),
        },
    )
    .expect("baseline");
    assert_eq!(baseline.untracked_files, vec!["untracked"]);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    let args = successful_args
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone();
    assert_eq!(
        &args[..6],
        &[
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "ls-files",
            "--others",
        ]
    );
    assert_eq!(
        successful_locks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone(),
        Some("0".to_string())
    );
    let recorded = retries.lock().unwrap_or_else(|poison| poison.into_inner());
    assert_eq!(recorded.len(), 1);
    assert_eq!(
        recorded[0].args,
        vec![
            "ls-files".to_string(),
            "--others".to_string(),
            "--exclude-standard".to_string(),
            "-z".to_string()
        ]
    );
    assert_eq!(recorded[0].cwd, f.repo_root);
    assert_eq!(recorded[0].timeout_ms, 50);
    assert_eq!(
        std::fs::read(f.repo_root.join(".git/index")).expect("index"),
        index_before
    );
}

#[test]
fn untracked_content_over_an_injected_1_kib_cap_is_refused_before_rendering() {
    let f = setup();
    std::fs::write(f.repo_root.join("large"), vec![0u8; 1025]).expect("large");
    let error = capture_baseline(&f.repo_root, 1024).expect_err("must fail");
    assert!(matches!(error, IsolationError::BaselineTooLarge { .. }));
}

#[test]
fn staged_and_unstaged_rendered_output_obey_the_cap() {
    let f = setup();
    std::fs::write(f.repo_root.join("modify"), "x".repeat(2048)).expect("modify");
    assert!(capture_baseline(&f.repo_root, 1024).is_err());
    git(&f.repo_root, &["add", "modify"]).expect("add");
    assert!(capture_baseline(&f.repo_root, 1024).is_err());
}

#[test]
fn ignored_child_files_never_enter_the_delta() {
    let f = setup();
    std::fs::write(f.repo_root.join(".gitignore"), "ignored\n").expect("gitignore");
    git(&f.repo_root, &["add", ".gitignore"]).expect("add");
    git(&f.repo_root, &["commit", "-m", "ignore"]).expect("commit");
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let isolated = child(&f.repo_root, &f.root);
    std::fs::write(isolated.join("ignored"), "secret\n").expect("ignored");
    let result = capture_delta_patch(&isolated, &baseline).expect("delta");
    assert_eq!(result.root_patch, "");
}

#[test]
fn submodules_are_excluded_from_nested_discovery() {
    let f = setup();
    let sub = repo();
    git(
        &f.repo_root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &sub.repo_root.to_string_lossy(),
            "libs/sub",
        ],
    )
    .expect("submodule add");
    git(&f.repo_root, &["commit", "-am", "submodule"]).expect("commit");
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    assert!(baseline.nested.is_empty());
}

#[test]
fn git_failures_retain_command_exit_code_and_stderr() {
    let f = setup();
    let error = run_git(
        &str_args(&["rev-parse", "--verify", "missing-ref"]),
        &GitOptions::new(f.repo_root.clone()),
    )
    .expect_err("must fail");
    match error {
        IsolationError::Git { code, stderr, .. } => {
            assert_ne!(code, 0);
            assert!(stderr.contains("fatal"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    let _ = RepoBaseline {
        repo_root: f.repo_root.clone(),
        head_commit: String::new(),
        staged: String::new(),
        unstaged: String::new(),
        untracked_files: Vec::new(),
        untracked_patch: String::new(),
    };
}
