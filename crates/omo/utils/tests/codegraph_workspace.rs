use std::fs;
use std::path::Path;
use std::process::Command;

use pretty_assertions::assert_eq;
use tempfile::tempdir;
use utils::codegraph::*;

#[test]
fn sanitize_base_formats_names() {
    let result = sanitize_base("my repo:../with spaces");
    assert_eq!(result, "my-repo-..-with-spaces");
}

#[test]
fn prepare_workspace_preserves_existing_in_project() {
    let ws_dir = tempdir().unwrap();
    let workspace = ws_dir.path();
    let codegraph_dir = workspace.join(".codegraph");
    fs::create_dir_all(&codegraph_dir).unwrap();
    fs::write(codegraph_dir.join("keep.txt"), "keep").unwrap();

    let home = tempdir().unwrap();
    let result = prepare_codegraph_workspace(
        workspace,
        &PrepareCodegraphWorkspaceOptions {
            home_dir: Some(home.path().to_path_buf()),
            ..Default::default()
        },
    );

    assert_eq!(result.mode, CodegraphWorkspaceMode::InProject);
    assert_eq!(result.linked, false);
    assert_eq!(
        fs::read_to_string(workspace.join(".codegraph").join("keep.txt")).unwrap(),
        "keep"
    );
}

#[test]
fn prepare_workspace_creates_global_project_store_and_symlink() {
    let ws_dir = tempdir().unwrap();
    let workspace = ws_dir.path();
    let home = tempdir().unwrap();
    let home_path = home.path();

    let result = prepare_codegraph_workspace(
        workspace,
        &PrepareCodegraphWorkspaceOptions {
            home_dir: Some(home_path.to_path_buf()),
            ..Default::default()
        },
    );

    assert_eq!(result.mode, CodegraphWorkspaceMode::GlobalLinked);
    assert_eq!(result.linked, true);

    let link_target = fs::read_link(workspace.join(".codegraph")).unwrap();
    assert!(
        link_target.to_string_lossy().contains(
            &home_path
                .join(".maho")
                .join("codegraph")
                .join("projects")
                .to_string_lossy()
                .to_string()
        )
    );

    let metadata_str = fs::read_to_string(result.data_dir.join("source.json")).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata_str).unwrap();
    assert_eq!(metadata["version"], 1);
    assert!(!metadata["sourceDir"].as_str().unwrap().is_empty());
}

#[test]
fn prepare_workspace_falls_back_when_symlink_fails() {
    let ws_dir = tempdir().unwrap();
    let workspace = ws_dir.path();
    let home = tempdir().unwrap();

    let fail_symlink = |_t: &Path, _l: &Path, _p: &str| -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "blocked",
        ))
    };

    let result = prepare_codegraph_workspace(
        workspace,
        &PrepareCodegraphWorkspaceOptions {
            home_dir: Some(home.path().to_path_buf()),
            symlink: Some(&fail_symlink),
            ..Default::default()
        },
    );

    assert_eq!(result.mode, CodegraphWorkspaceMode::InPlaceFallback);
    assert_eq!(result.linked, false);
}

#[test]
fn prepare_workspace_falls_back_for_wrong_target_symlink() {
    let ws_dir = tempdir().unwrap();
    let workspace = ws_dir.path();
    let wrong = tempdir().unwrap();
    let home = tempdir().unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink(wrong.path(), workspace.join(".codegraph")).unwrap();

    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(wrong.path(), workspace.join(".codegraph")).unwrap();

    let result = prepare_codegraph_workspace(
        workspace,
        &PrepareCodegraphWorkspaceOptions {
            home_dir: Some(home.path().to_path_buf()),
            ..Default::default()
        },
    );

    assert_eq!(result.mode, CodegraphWorkspaceMode::InPlaceFallback);
    assert_eq!(result.linked, false);
}

#[test]
fn ensure_codegraph_gitignored_standard_repo() {
    let ws_dir = tempdir().unwrap();
    let workspace = ws_dir.path();

    let init_status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(workspace)
        .status()
        .unwrap();
    assert!(init_status.success());

    let first = ensure_codegraph_gitignored(workspace);
    let second = ensure_codegraph_gitignored(workspace);

    assert_eq!(first, true);
    assert_eq!(second, true);

    let exclude_content =
        fs::read_to_string(workspace.join(".git").join("info").join("exclude")).unwrap();
    let lines: Vec<&str> = exclude_content.lines().collect();
    let occurrences = lines.iter().filter(|&&line| line == ".codegraph").count();
    assert_eq!(occurrences, 1);
    assert!(!workspace.join(".gitignore").exists());
}

#[test]
fn ensure_codegraph_gitignored_empty_git_dir() {
    let ws_dir = tempdir().unwrap();
    let workspace = ws_dir.path();
    fs::create_dir_all(workspace.join(".git")).unwrap();

    let result = ensure_codegraph_gitignored(workspace);
    assert_eq!(result, false);
    assert!(!workspace.join(".git").join("info").join("exclude").exists());
}

#[test]
fn ensure_codegraph_gitignored_nested_git_dir_rejected() {
    let parent_dir = tempdir().unwrap();
    let parent = parent_dir.path();
    let workspace = parent.join("nested");
    fs::create_dir_all(&workspace).unwrap();

    let init_status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(init_status.success());

    fs::create_dir_all(workspace.join(".git")).unwrap();
    let parent_exclude_path = parent.join(".git").join("info").join("exclude");
    let parent_before = fs::read_to_string(&parent_exclude_path).unwrap_or_default();

    let result = ensure_codegraph_gitignored(&workspace);
    assert_eq!(result, false);
    assert!(!workspace.join(".git").join("info").join("exclude").exists());
    let parent_after = fs::read_to_string(&parent_exclude_path).unwrap_or_default();
    assert_eq!(parent_before, parent_after);
}

#[test]
fn ensure_codegraph_gitignored_nested_gitdir_pointer_rejected() {
    let parent_dir = tempdir().unwrap();
    let parent = parent_dir.path();
    let workspace = parent.join("nested");
    fs::create_dir_all(&workspace).unwrap();

    let init_status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(parent)
        .status()
        .unwrap();
    assert!(init_status.success());

    fs::write(workspace.join(".git"), "gitdir: ../.git\n").unwrap();
    let parent_exclude_path = parent.join(".git").join("info").join("exclude");
    let parent_before = fs::read_to_string(&parent_exclude_path).unwrap_or_default();

    let result = ensure_codegraph_gitignored(&workspace);
    assert_eq!(result, false);
    let parent_after = fs::read_to_string(&parent_exclude_path).unwrap_or_default();
    assert_eq!(parent_before, parent_after);
}

#[test]
fn ensure_codegraph_gitignored_linked_worktree() {
    let root_dir = tempdir().unwrap();
    let root = root_dir.path();
    let main = root.join("main");
    let linked = root.join("linked");

    fs::create_dir_all(&main).unwrap();
    let init_status = Command::new("git")
        .args(["init", "-q"])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(init_status.success());

    let commit_status = Command::new("git")
        .args([
            "-c",
            "user.name=OMO Test",
            "-c",
            "user.email=omo@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(commit_status.success());

    let worktree_status = Command::new("git")
        .args([
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
        ])
        .current_dir(&main)
        .status()
        .unwrap();
    assert!(worktree_status.success());

    let first = ensure_codegraph_gitignored(&linked);
    let second = ensure_codegraph_gitignored(&linked);
    assert_eq!(first, true);
    assert_eq!(second, true);
}

#[test]
fn ensure_codegraph_gitignored_non_git() {
    let ws_dir = tempdir().unwrap();
    let workspace = ws_dir.path();

    let result = ensure_codegraph_gitignored(workspace);
    assert_eq!(result, false);
    assert!(!workspace.join(".git").exists());
}

#[test]
fn prune_store_least_recently_used() {
    let home = tempdir().unwrap();
    let projects = home.path().join(".maho").join("codegraph").join("projects");
    let old_proj = projects.join("old");
    let new_proj = projects.join("new");
    fs::create_dir_all(&old_proj).unwrap();
    fs::create_dir_all(&new_proj).unwrap();

    fs::write(old_proj.join("blob"), "x".repeat(20)).unwrap();
    fs::write(new_proj.join("blob"), "x".repeat(20)).unwrap();

    let result = prune_codegraph_store(&PruneCodegraphStoreOptions {
        home_dir: Some(home.path().to_path_buf()),
        max_age_days: 999,
        max_bytes: 25,
        now_ms: None,
        prune_missing_sources: None,
    });

    assert_eq!(result.removed.len(), 1);
    assert!(result.remaining_bytes <= 25);
}

#[test]
fn prune_store_missing_sources() {
    let home = tempdir().unwrap();
    let home_path = home.path();
    let live = tempdir().unwrap();
    let dead = tempdir().unwrap();

    let live_prep = prepare_codegraph_workspace(
        live.path(),
        &PrepareCodegraphWorkspaceOptions {
            home_dir: Some(home_path.to_path_buf()),
            ..Default::default()
        },
    );
    let dead_prep = prepare_codegraph_workspace(
        dead.path(),
        &PrepareCodegraphWorkspaceOptions {
            home_dir: Some(home_path.to_path_buf()),
            ..Default::default()
        },
    );

    fs::write(live_prep.data_dir.join("blob"), "live").unwrap();
    fs::write(dead_prep.data_dir.join("blob"), "dead").unwrap();

    drop(dead);

    let result = prune_codegraph_store(&PruneCodegraphStoreOptions {
        home_dir: Some(home_path.to_path_buf()),
        max_age_days: 999,
        max_bytes: 100_000,
        now_ms: None,
        prune_missing_sources: Some(true),
    });

    assert!(
        result
            .removed
            .contains(&dead_prep.data_dir.to_string_lossy().into_owned())
    );
    assert!(
        !result
            .removed
            .contains(&live_prep.data_dir.to_string_lossy().into_owned())
    );
    assert!(!dead_prep.data_dir.exists());
    assert!(live_prep.data_dir.exists());
}

#[test]
fn prune_dead_project_stores_ignores_unreadable_cache() {
    let home = tempdir().unwrap();
    let home_path = home.path();
    let live = tempdir().unwrap();
    let dead = tempdir().unwrap();

    let live_prep = prepare_codegraph_workspace(
        live.path(),
        &PrepareCodegraphWorkspaceOptions {
            home_dir: Some(home_path.to_path_buf()),
            ..Default::default()
        },
    );
    let dead_prep = prepare_codegraph_workspace(
        dead.path(),
        &PrepareCodegraphWorkspaceOptions {
            home_dir: Some(home_path.to_path_buf()),
            ..Default::default()
        },
    );

    drop(dead);

    let result = prune_dead_codegraph_project_stores(&PruneDeadCodegraphProjectStoresOptions {
        home_dir: Some(home_path.to_path_buf()),
    });

    assert!(
        result
            .removed
            .contains(&dead_prep.data_dir.to_string_lossy().into_owned())
    );
    assert!(
        !result
            .removed
            .contains(&live_prep.data_dir.to_string_lossy().into_owned())
    );
    assert!(!dead_prep.data_dir.exists());
    assert!(live_prep.data_dir.exists());
}

#[test]
fn prune_dead_project_stores_ignores_malformed_metadata() {
    let home = tempdir().unwrap();
    let home_path = home.path();
    let live = tempdir().unwrap();
    let dead = tempdir().unwrap();

    let malformed_prep = prepare_codegraph_workspace(
        live.path(),
        &PrepareCodegraphWorkspaceOptions {
            home_dir: Some(home_path.to_path_buf()),
            ..Default::default()
        },
    );
    let dead_prep = prepare_codegraph_workspace(
        dead.path(),
        &PrepareCodegraphWorkspaceOptions {
            home_dir: Some(home_path.to_path_buf()),
            ..Default::default()
        },
    );

    fs::write(malformed_prep.data_dir.join("source.json"), "{").unwrap();
    drop(dead);

    let result = prune_dead_codegraph_project_stores(&PruneDeadCodegraphProjectStoresOptions {
        home_dir: Some(home_path.to_path_buf()),
    });

    assert!(
        result
            .removed
            .contains(&dead_prep.data_dir.to_string_lossy().into_owned())
    );
    assert!(
        !result
            .removed
            .contains(&malformed_prep.data_dir.to_string_lossy().into_owned())
    );
    assert!(!dead_prep.data_dir.exists());
    assert!(malformed_prep.data_dir.exists());
}

#[test]
fn daemon_lock_parsing_variants() {
    let full_json = r#"{
        "pid": 44375,
        "socketPath": "/tmp/daemon.sock",
        "startedAt": 1784615252733,
        "version": "1.4.1"
    }"#;
    let parsed = parse_daemon_lock(full_json).unwrap();
    assert_eq!(
        parsed,
        CodegraphDaemonLock {
            pid: 44375,
            socket_path: Some("/tmp/daemon.sock".to_string()),
            started_at: Some(1784615252733),
            version: Some("1.4.1".to_string()),
        }
    );

    let legacy = "605\n";
    let parsed_legacy = parse_daemon_lock(legacy).unwrap();
    assert_eq!(
        parsed_legacy,
        CodegraphDaemonLock {
            pid: 605,
            socket_path: None,
            started_at: None,
            version: None,
        }
    );

    assert_eq!(parse_daemon_lock(""), None);
    assert_eq!(parse_daemon_lock("   \n"), None);
    assert_eq!(parse_daemon_lock("not-a-pid-not-json\n"), None);
    assert_eq!(parse_daemon_lock(r#"{"pid": -1}"#), None);
    assert_eq!(parse_daemon_lock(r#"{"socketPath": "abc"}"#), None);
}

#[test]
fn evaluate_daemon_staleness_states() {
    let project_dir = tempdir().unwrap();
    let proj_path = project_dir.path();

    let absent = evaluate_daemon_staleness(602, proj_path);
    assert_eq!(
        absent,
        CodegraphDaemonStaleness {
            stale: true,
            reason: DaemonStalenessReason::LockAbsent,
        }
    );
    assert_eq!(absent.reason.as_str(), "lock-absent");

    let lock_dir = proj_path.join(".codegraph");
    fs::create_dir_all(&lock_dir).unwrap();
    let lock_file = lock_dir.join("daemon.pid");

    fs::write(
        &lock_file,
        r#"{"pid": 601, "socketPath": "/tmp/s.sock", "startedAt": 1000, "version": "1.5.0"}"#,
    )
    .unwrap();
    let matched = evaluate_daemon_staleness(601, proj_path);
    assert_eq!(
        matched,
        CodegraphDaemonStaleness {
            stale: false,
            reason: DaemonStalenessReason::LockPidMatch,
        }
    );
    assert_eq!(matched.reason.as_str(), "lock-pid-match");

    let mismatch = evaluate_daemon_staleness(999, proj_path);
    assert_eq!(
        mismatch,
        CodegraphDaemonStaleness {
            stale: true,
            reason: DaemonStalenessReason::LockPidMismatch,
        }
    );
    assert_eq!(mismatch.reason.as_str(), "lock-pid-mismatch");

    fs::write(&lock_file, "not-a-pid-not-json\n").unwrap();
    let unparseable = evaluate_daemon_staleness(604, proj_path);
    assert_eq!(
        unparseable,
        CodegraphDaemonStaleness {
            stale: false,
            reason: DaemonStalenessReason::LockUnparseable,
        }
    );
    assert_eq!(unparseable.reason.as_str(), "lock-unparseable");

    fs::write(&lock_file, "605\n").unwrap();
    let legacy_match = evaluate_daemon_staleness(605, proj_path);
    assert_eq!(
        legacy_match,
        CodegraphDaemonStaleness {
            stale: false,
            reason: DaemonStalenessReason::LockPidMatch,
        }
    );

    let ancestor_dir = tempdir().unwrap();
    let ancestor_path = ancestor_dir.path();
    let nested_dir = ancestor_path.join("sub").join("dir");
    fs::create_dir_all(&nested_dir).unwrap();
    let ancestor_lock_dir = ancestor_path.join(".codegraph");
    fs::create_dir_all(&ancestor_lock_dir).unwrap();
    fs::write(
        ancestor_lock_dir.join("daemon.pid"),
        r#"{"pid": 607, "version": "1.5.0"}"#,
    )
    .unwrap();
    let ancestor_match = evaluate_daemon_staleness(607, &nested_dir);
    assert_eq!(
        ancestor_match,
        CodegraphDaemonStaleness {
            stale: false,
            reason: DaemonStalenessReason::LockPidMatch,
        }
    );
}
