use std::fs;
use std::path::Path;

use super::read_head_from_files;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn layout(root: &Path) {
    for dir in [".git/objects", ".git/refs/heads"] {
        fs::create_dir_all(root.join(dir)).expect("dirs");
    }
    fs::write(root.join(".git/config"), "[core]\n\tbare = false\n").expect("config");
    fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").expect("head");
}

#[test]
fn reads_a_loose_branch_ref() {
    let root = tempfile::tempdir().expect("tempdir");
    layout(root.path());
    fs::write(root.path().join(".git/refs/heads/main"), format!("{A}\n")).expect("ref");
    assert_eq!(read_head_from_files(root.path()).as_deref(), Some(A));
}

#[test]
fn reads_a_detached_head() {
    let root = tempfile::tempdir().expect("tempdir");
    layout(root.path());
    fs::write(root.path().join(".git/HEAD"), format!("{B}\n")).expect("head");
    assert_eq!(read_head_from_files(root.path()).as_deref(), Some(B));
}

#[test]
fn falls_back_to_packed_refs_and_refuses_duplicates() {
    let root = tempfile::tempdir().expect("tempdir");
    layout(root.path());
    let packed = root.path().join(".git/packed-refs");
    fs::write(
        &packed,
        format!("# pack-refs\n{A} refs/heads/main\n^{B}\n{B} refs/heads/other\n"),
    )
    .expect("packed");
    assert_eq!(read_head_from_files(root.path()).as_deref(), Some(A));
    fs::write(
        &packed,
        format!("{A} refs/heads/main\n{B} refs/heads/main\n"),
    )
    .expect("packed");
    assert_eq!(read_head_from_files(root.path()), None);
}

#[test]
fn defers_to_git_for_unfamiliar_layouts() {
    let root = tempfile::tempdir().expect("tempdir");
    layout(root.path());
    fs::write(root.path().join(".git/refs/heads/main"), format!("{A}\n")).expect("ref");
    let git = root.path().join(".git");

    fs::write(git.join("config"), "[include]\n\tpath = other\n").expect("config");
    assert_eq!(read_head_from_files(root.path()), None);
    fs::write(
        git.join("config"),
        "[extensions]\n\tworktreeConfig = true\n",
    )
    .expect("config");
    assert_eq!(read_head_from_files(root.path()), None);
    fs::write(git.join("config"), "[core]\n").expect("config");
    assert_eq!(read_head_from_files(root.path()).as_deref(), Some(A));

    fs::write(git.join("config.worktree"), "").expect("worktree config");
    assert_eq!(read_head_from_files(root.path()), None);
    fs::remove_file(git.join("config.worktree")).expect("remove");

    fs::create_dir(git.join("reftable")).expect("reftable");
    assert_eq!(read_head_from_files(root.path()), None);
    fs::remove_dir(git.join("reftable")).expect("remove");

    fs::write(git.join("HEAD"), "ref: refs/heads/../x\n").expect("head");
    assert_eq!(read_head_from_files(root.path()), None);
}

#[test]
fn a_missing_repository_defers_to_git() {
    let root = tempfile::tempdir().expect("tempdir");
    assert_eq!(read_head_from_files(root.path()), None);
}

#[test]
fn head_matches_git_rev_parse_after_a_commit() {
    let (_temp, repo) = {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = crate::git::GitMemoryRepo::open(temp.path(), "agent-one").expect("repo");
        (temp, repo)
    };
    repo.init(Some(crate::git::InitializeGitRepoOptions::default()))
        .expect("init");
    let resolved = repo.head().expect("head").expect("a commit");
    let by_files = read_head_from_files(&repo.dir);
    assert!(by_files.is_none() || by_files.as_deref() == Some(resolved.as_str()));
}
