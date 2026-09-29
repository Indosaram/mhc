use pretty_assertions::assert_eq;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use super::errors::GitError;
use super::exec::{GitExecOptions, system_git_exec};
use super::path_state::{GitIndexIdentity, GitWorktreeFileIdentity, GitWorktreeIdentity};
use super::repo::GitMemoryRepo;
use super::repo_types::{GitCommitAuthor, GitSeedFile, InitializeGitRepoOptions};

fn author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "path-state-agent".to_string(),
        author_name: "Path State Agent".to_string(),
        author_email: None,
    }
}

fn fixture(seed_files: Vec<GitSeedFile>) -> (tempfile::TempDir, GitMemoryRepo) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(temp_dir.path(), "path-state-agent").expect("repo");
    let opts = InitializeGitRepoOptions {
        author_name: Some("Path State Agent".to_string()),
        seed_files,
        install_hooks: None,
    };
    repo.init(opts).expect("init");
    (temp_dir, repo)
}

#[test]
fn test_captures_stage_zero_index_and_worktree_identities() {
    let (_temp, repo) = fixture(vec![GitSeedFile {
        relative_path: "tracked.md".to_string(),
        content: "base\n".to_string(),
    }]);

    let clean = repo.path_state.capture("tracked.md").expect("clean");
    let missing = repo.path_state.capture("missing.md").expect("missing");
    let captured = repo
        .path_state
        .capture_all(&["tracked.md", "missing.md", "tracked.md"])
        .expect("captured");

    assert!(clean.index.is_some());
    assert_eq!(missing.index, None);
    assert_eq!(missing.worktree, GitWorktreeIdentity::Missing);
    assert_eq!(captured.len(), 2);
    assert_eq!(captured.get("tracked.md"), Some(&clean));

    fs::write(repo.dir.join("tracked.md"), "worktree\n").expect("write");
    let worktree_dirty = repo.path_state.capture("tracked.md").expect("dirty");
    assert_eq!(worktree_dirty.index, clean.index);
    assert_ne!(worktree_dirty.worktree, clean.worktree);
}

#[cfg(unix)]
#[test]
fn test_captures_and_restores_executable_modes() {
    use std::os::unix::fs::PermissionsExt;

    let (_temp, repo) = fixture(vec![GitSeedFile {
        relative_path: "script.sh".to_string(),
        content: "#!/bin/sh\n".to_string(),
    }]);

    let script_path = repo.dir.join("script.sh");
    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o755)).expect("chmod");
    repo.commit_write(&["script.sh"], "make executable", &author())
        .expect("commit");

    let state = repo.path_state.capture("script.sh").expect("capture");
    assert_eq!(
        state.index.as_ref().map(|i| i.mode.as_str()),
        Some("100755")
    );
    match &state.worktree {
        GitWorktreeIdentity::File(f) => assert_eq!(f.mode, 0o755),
        _ => panic!("expected file"),
    }

    fs::set_permissions(&script_path, fs::Permissions::from_mode(0o644)).expect("chmod");
    if let GitWorktreeIdentity::File(ref f) = state.worktree {
        repo.path_state
            .write_worktree("script.sh", f)
            .expect("write");
    }
    let restored_mode = fs::symlink_metadata(&script_path).expect("meta").mode() & 0o777;
    assert_eq!(restored_mode, 0o755);
}

#[test]
fn test_hashes_and_writes_blobs() {
    let (_temp, repo) = fixture(vec![]);
    let content = "unique-blob-content\n";

    let raw = repo
        .path_state
        .hash_worktree_blob(content, true)
        .expect("raw");
    let filtered = repo
        .path_state
        .hash_index_blob("filtered.txt", content, true)
        .expect("filtered");

    assert_eq!(raw, filtered);
    assert_eq!(repo.path_state.read_blob(&raw).expect("read"), content);
}

#[test]
fn test_sets_and_removes_index_entries() {
    let (_temp, repo) = fixture(vec![GitSeedFile {
        relative_path: "entry.md".to_string(),
        content: "base\n".to_string(),
    }]);

    let before = repo.path_state.capture("entry.md").expect("before");
    let replacement = repo
        .path_state
        .hash_index_blob("entry.md", "index-only\n", true)
        .expect("hash");

    repo.path_state
        .set_index(
            "entry.md",
            &GitIndexIdentity {
                mode: "100644".to_string(),
                oid: replacement.clone(),
            },
        )
        .expect("set");

    let changed = repo.path_state.capture("entry.md").expect("changed");
    assert_eq!(changed.index.as_ref().map(|i| &i.oid), Some(&replacement));
    assert_eq!(changed.worktree, before.worktree);

    repo.path_state.remove_index("entry.md").expect("remove");
    let removed = repo.path_state.capture("entry.md").expect("removed");
    assert_eq!(removed.index, None);
    assert_eq!(
        fs::read_to_string(repo.dir.join("entry.md")).expect("read"),
        "base\n"
    );
}

#[test]
fn test_atomically_materializes_and_removes_worktree_files() {
    let (_temp, repo) = fixture(vec![]);
    let oid = repo
        .path_state
        .hash_worktree_blob("materialized\n", true)
        .expect("hash");
    let identity = GitWorktreeFileIdentity { mode: 0o640, oid };

    repo.path_state
        .write_worktree("nested/a b.md", &identity)
        .expect("write 1");
    repo.path_state
        .write_worktree("nested/a b.md", &identity)
        .expect("write 2");
    assert_eq!(
        fs::read_to_string(repo.dir.join("nested/a b.md")).expect("read"),
        "materialized\n"
    );

    repo.path_state
        .remove_worktree("nested/a b.md")
        .expect("rm 1");
    repo.path_state
        .remove_worktree("nested/a b.md")
        .expect("rm 2");
    let state = repo.path_state.capture("nested/a b.md").expect("capture");
    assert_eq!(state.worktree, GitWorktreeIdentity::Missing);
}

#[test]
fn test_rejects_traversal_and_invalid_paths() {
    let (_temp, repo) = fixture(vec![]);
    assert!(repo.path_state.capture("../outside.md").is_err());
    assert!(repo.path_state.capture("C:\\outside.md").is_err());
    assert!(repo.path_state.capture("nested/.git/config").is_err());
}

#[test]
fn test_rejects_unsupported_index_modes() {
    let (_temp, repo) = fixture(vec![]);
    let head = repo.head().expect("head").expect("some head");

    let exec = system_git_exec();
    let opts = GitExecOptions {
        cwd: repo.dir.clone(),
        timeout_ms: 30000,
        env: std::collections::BTreeMap::new(),
        stdin: None,
    };
    exec.run(
        &[
            "update-index".to_string(),
            "--add".to_string(),
            "--cacheinfo".to_string(),
            "160000".to_string(),
            head,
            "submodule".to_string(),
        ],
        &opts,
    )
    .expect("update-index");

    let err = repo.path_state.capture("submodule").unwrap_err();
    match err {
        GitError::PathState(msg) => assert!(msg.contains("160000")),
        _ => panic!("expected PathState error"),
    }
}
