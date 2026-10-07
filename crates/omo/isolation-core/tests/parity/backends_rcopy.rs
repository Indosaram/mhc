use isolation_core::backends::git_fixture::{git, repo};
use isolation_core::test_support::fixture;
use isolation_core::{IsolationBackend, IsolationContext, RcopyBackend};

fn context(base_dir: &std::path::Path, max_copy_bytes: Option<u64>) -> IsolationContext {
    IsolationContext {
        id: "test".to_string(),
        base_dir: base_dir.to_path_buf(),
        cross_device: false,
        max_copy_bytes,
    }
}

#[test]
fn rcopy_seeds_staged_unstaged_and_nul_delimited_untracked_files_without_ignored_files() {
    let f = repo();
    let lower = f.repo_root.clone();
    let merged = f.root.join("merged");
    std::fs::write(lower.join("staged"), "stage\n").expect("staged");
    git(&lower, &["add", "staged"]).expect("add");
    std::fs::write(lower.join("tracked"), "dirty\n").expect("tracked");
    // The name must force ls-files' NUL delimitation to matter: a newline breaks
    // the non-z output outright (POSIX), and git C-quotes non-ASCII names in the
    // non-z output, so either form proves the -z round-trip.
    let name = if cfg!(windows) {
        "untracked-한글".to_string()
    } else {
        "untracked\nname".to_string()
    };
    std::fs::write(lower.join(&name), "untracked").expect("untracked");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(lower.join(&name), std::fs::Permissions::from_mode(0o751))
            .expect("mode");
    }
    filetime::set_file_mtime(
        lower.join(&name),
        filetime::FileTime::from_unix_time(1234567890, 0),
    )
    .expect("mtime");
    std::fs::write(lower.join("ignored"), "ignored").expect("ignored");
    let backend = RcopyBackend;
    backend
        .start(&lower, &merged, &context(&f.root, None))
        .expect("start");
    assert_eq!(
        std::fs::read_to_string(merged.join("tracked")).expect("tracked"),
        "dirty\n"
    );
    assert_eq!(
        git(&merged, &["diff", "--cached", "--name-only"]).expect("cached"),
        "staged"
    );
    assert_eq!(
        std::fs::read_to_string(merged.join(&name)).expect("untracked"),
        "untracked"
    );
    assert!(!merged.join("ignored").exists());
    let mtime = std::fs::metadata(merged.join(&name))
        .expect("stat")
        .modified()
        .expect("modified")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_millis();
    assert_eq!(mtime, 1_234_567_890_000);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(merged.join(&name))
                .expect("stat")
                .permissions()
                .mode()
                & 0o777,
            0o751
        );
    }
    backend.stop(&merged).expect("stop");
    assert!(!git(&lower, &["worktree", "list", "--porcelain"])
        .expect("worktrees")
        .contains(&merged.to_string_lossy().into_owned()));
}

#[test]
fn rcopy_plain_copy_preserves_symlinks_refuses_existing_destinations_and_enforces_byte_ceiling() {
    let f = fixture();
    let lower = f.repo_root.clone();
    std::fs::write(lower.join("data"), "123456").expect("data");
    std::os::unix::fs::symlink("data", lower.join("link")).expect("symlink");
    let backend = RcopyBackend;
    let error = backend
        .start(&lower, &f.root.join("too-big"), &context(&f.root, Some(5)))
        .expect_err("must fail");
    assert!(error.to_string().contains('6'));
    let merged = f.root.join("merged");
    backend
        .start(&lower, &merged, &context(&f.root, Some(100)))
        .expect("start");
    assert!(std::fs::symlink_metadata(merged.join("link"))
        .expect("lstat")
        .file_type()
        .is_symlink());
    assert!(backend
        .start(&lower, &merged, &context(&f.root, Some(100)))
        .is_err());
    backend.stop(&merged).expect("stop");
}
