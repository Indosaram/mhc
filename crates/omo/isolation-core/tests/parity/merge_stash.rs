use isolation_core::backends::git_fixture::{git, repo};
use isolation_core::{stash_pop, stash_push};

#[test]
fn a_no_op_stash_push_neither_creates_nor_pops_the_users_stash() {
    let f = repo();
    // An unrelated pre-existing user stash must survive the merge's stash cycle.
    std::fs::write(f.repo_root.join("tracked"), "user stash\n").expect("write");
    git(&f.repo_root, &["stash", "push", "-m", "user"]).expect("stash");
    let user_stash = git(&f.repo_root, &["rev-parse", "--verify", "refs/stash"]).expect("stash sha");
    // Dirt inside a submodule: status is nonempty, but nothing stashable exists for git.
    let nested = repo();
    git(
        &f.repo_root,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &nested.repo_root.to_string_lossy(),
            "nested",
        ],
    )
    .expect("submodule add");
    git(&f.repo_root, &["commit", "-m", "submodule"]).expect("commit");
    std::fs::write(f.repo_root.join("nested/tracked"), "dirty inside submodule\n").expect("dirty");
    let stashed = stash_push(&f.repo_root).expect("stash push");
    assert!(stashed.is_none());
    if let Some(stashed) = stashed {
        let _ = stash_pop(&f.repo_root, &stashed);
    }
    assert_eq!(
        git(&f.repo_root, &["rev-parse", "--verify", "refs/stash"]).expect("stash sha"),
        user_stash
    );
    assert_eq!(
        std::fs::read_to_string(f.repo_root.join("tracked")).expect("tracked"),
        "base\n"
    );
}

#[test]
fn a_created_stash_is_restored_by_identity_leaving_later_entries_alone() {
    let f = repo();
    std::fs::write(f.repo_root.join("tracked"), "user keeps this\n").expect("write");
    git(&f.repo_root, &["stash", "push", "-m", "user"]).expect("stash");
    let user_stash = git(&f.repo_root, &["rev-parse", "--verify", "refs/stash"]).expect("stash sha");
    // The merge's own dirt: this is what the cycle must stash and restore.
    std::fs::write(f.repo_root.join("tracked"), "merge dirt\n").expect("dirt");
    let created = stash_push(&f.repo_root).expect("push").expect("created stash");
    // Another entry lands on top between push and pop; ours must still be the one restored.
    std::fs::write(f.repo_root.join("unrelated"), "unrelated\n").expect("unrelated");
    git(&f.repo_root, &["add", "unrelated"]).expect("add");
    git(&f.repo_root, &["stash", "push", "-m", "other process"]).expect("other stash");
    let other_stash = git(&f.repo_root, &["rev-parse", "--verify", "refs/stash"]).expect("stash sha");
    let warning = stash_pop(&f.repo_root, &created).expect("pop");
    assert!(warning.is_none());
    assert_eq!(
        std::fs::read_to_string(f.repo_root.join("tracked")).expect("tracked"),
        "merge dirt\n"
    );
    assert_eq!(
        git(&f.repo_root, &["rev-parse", "--verify", "refs/stash"]).expect("stash sha"),
        other_stash
    );
    assert_ne!(other_stash, user_stash);
    assert!(git(&f.repo_root, &["stash", "list"]).expect("stash list").contains("other process"));
}
