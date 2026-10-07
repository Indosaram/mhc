use std::path::{Path, PathBuf};

use isolation_core::backends::git_fixture::{git, repo};
use isolation_core::test_support::fixture;
use isolation_core::{
    cleanup_isolation, detach_git_dir, ensure_isolation, scan_nested_git_dirs, BackendRef,
    DetachOutcome, EnsureIsolationOptions, IsolationError,
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

#[test]
fn linked_worktree_detach_uses_private_metadata_and_alternates_and_removes_own_registration() {
    let f = repo();
    let source = f.repo_root.clone();
    git(&source, &["config", "remote.origin.url", "https://example.invalid/private"])
        .expect("remote");
    git(&source, &["config", "core.hooksPath", "/untrusted/hooks"]).expect("hooks");
    let lower = f.root.join("linked");
    git(&source, &["worktree", "add", "--detach", &lower.to_string_lossy(), "HEAD"]).expect("worktree");
    let merged = f.root.join("merged");
    git(&lower, &["worktree", "add", "--detach", &merged.to_string_lossy(), "HEAD"]).expect("worktree");
    let admin = PathBuf::from(
        std::fs::read_to_string(merged.join(".git"))
            .expect("gitfile")
            .trim()
            .strip_prefix("gitdir: ")
            .expect("gitdir prefix")
            .to_string(),
    );
    let common = git(&lower, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .expect("common dir");
    assert_eq!(
        detach_git_dir(&merged, Path::new(&common)).expect("detach"),
        DetachOutcome::Detached
    );
    let resolved = isolation_core::resolve_path(&merged.join(
        git(&merged, &["rev-parse", "--git-common-dir"]).expect("common dir"),
    ));
    assert_eq!(resolved, merged.join(".git"));
    assert_eq!(git(&merged, &["log", "-1", "--format=%s"]).expect("log"), "fixture");
    let worktrees = git(&source, &["worktree", "list", "--porcelain"]).expect("worktrees");
    assert!(!worktrees.contains(&merged.to_string_lossy().into_owned()));
    assert!(worktrees.contains(&lower.to_string_lossy().into_owned()));
    assert!(!admin.exists());
    let config = std::fs::read_to_string(merged.join(".git/config")).expect("config");
    assert!(!config.contains("origin"));
    assert!(!config.contains("hooksPath"));
    assert_eq!(git(&merged, &["config", "user.name"]).expect("user.name"), "Fixture");
}

#[test]
fn directory_metadata_removes_nested_locks_and_worktree_bare_configuration_and_absent_is_no_git() {
    let f = repo();
    std::fs::write(f.repo_root.join(".git/index.lock"), "stale").expect("index.lock");
    std::fs::create_dir_all(f.repo_root.join(".git/refs/heads/nested")).expect("nested refs");
    std::fs::write(f.repo_root.join(".git/refs/heads/nested/ref.lock"), "stale").expect("ref.lock");
    git(&f.repo_root, &["config", "core.worktree", &f.root.to_string_lossy()]).expect("worktree cfg");
    git(&f.repo_root, &["config", "core.bare", "false"]).expect("bare cfg");
    assert_eq!(
        detach_git_dir(&f.repo_root, &f.repo_root.join(".git")).expect("detach"),
        DetachOutcome::Independent
    );
    assert!(!f.repo_root.join(".git/index.lock").exists());
    assert!(!f.repo_root.join(".git/refs/heads/nested/ref.lock").exists());
    let config = std::fs::read_to_string(f.repo_root.join(".git/config")).expect("config");
    assert!(!config.contains("worktree"));
    assert!(!config.contains("bare"));
    let empty = f.root.join("empty");
    std::fs::create_dir_all(&empty).expect("empty");
    assert_eq!(
        detach_git_dir(&empty, &f.repo_root.join(".git")).expect("detach"),
        DetachOutcome::NoGit
    );
}

#[test]
fn copied_linked_metadata_never_deletes_the_source_worktree_admin() {
    let f = repo();
    let linked = f.root.join("linked");
    let merged = f.root.join("merged");
    git(&f.repo_root, &["worktree", "add", "--detach", &linked.to_string_lossy(), "HEAD"])
        .expect("worktree");
    copy_dir_all(&linked, &merged).expect("copy");
    detach_git_dir(&merged, &f.repo_root.join(".git")).expect("detach");
    assert_eq!(git(&linked, &["status", "--porcelain"]).expect("status"), "");
    assert!(git(&f.repo_root, &["worktree", "list", "--porcelain"])
        .expect("worktrees")
        .contains(&linked.to_string_lossy().into_owned()));
}

#[test]
fn nested_relative_submodule_metadata_remains_functional_and_external_metadata_rewrites_or_fails_closed() {
    let f = repo();
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
    let merged = f.root.join("merged");
    copy_dir_all(&f.repo_root, &merged).expect("copy");
    detach_git_dir(&merged, &f.repo_root.join(".git")).expect("detach");
    assert!(scan_nested_git_dirs(&merged)
        .expect("scan")
        .nested_git_rewritten
        .is_empty());
    assert_eq!(
        git(&merged.join("libs/sub"), &["status", "--porcelain"]).expect("status"),
        ""
    );
    let absolute = f.repo_root.join(".git/modules/libs/sub");
    assert!(absolute.is_absolute());
    std::fs::write(
        merged.join("libs/sub/.git"),
        format!("gitdir: {}\n", absolute.display()),
    )
    .expect("external gitfile");
    assert_eq!(
        scan_nested_git_dirs(&merged)
            .expect("scan")
            .nested_git_rewritten,
        vec!["libs/sub".to_string()]
    );
    assert_eq!(
        git(&merged.join("libs/sub"), &["status", "--porcelain"]).expect("status"),
        ""
    );
    let foreign = f.root.join("foreign");
    std::fs::create_dir_all(foreign.join("libs/sub")).expect("foreign");
    std::fs::write(
        foreign.join("libs/sub/.git"),
        format!("gitdir: {}\n", absolute.display()),
    )
    .expect("foreign gitfile");
    let error = scan_nested_git_dirs(&foreign).expect_err("must fail");
    assert!(error.is_unavailable());
    assert!(error.to_string().contains("libs/sub"));
}

#[test]
fn ensure_retries_one_inconsistent_clone_detaches_before_publication_then_refuses_repeated_corruption() {
    let f = repo();
    let starts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = std::sync::Arc::clone(&starts);
    let backend: BackendRef = std::sync::Arc::new(isolation_core::RcopyBackend);
    let wrapper: BackendRef = std::sync::Arc::new(WrappingBackend {
        inner: backend,
        counter: std::sync::Arc::clone(&counter),
        corrupt_once: true,
    });
    let handle = ensure_isolation(EnsureIsolationOptions {
        repo_root: f.repo_root.clone(),
        id: "retry".to_string(),
        preferred: None,
        backends: vec![wrapper],
        platform: None,
        home_dir: Some(f.home_dir.clone()),
        owner: None,
        max_copy_bytes: None,
    })
    .expect("handle");
    assert_eq!(starts.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert!(std::fs::metadata(handle.merged_dir.join(".git"))
        .expect("git dir")
        .is_dir());
    assert_eq!(git(&handle.merged_dir, &["status", "--porcelain"]).expect("status"), "");
    cleanup_isolation(&handle).expect("cleanup");
    let _ = IsolationError::other("placeholder");
}

#[test]
fn directory_metadata_rejects_external_symlinks_beneath_mutation_targets() {
    let f = repo();
    std::fs::remove_dir_all(f.repo_root.join(".git/objects")).expect("remove objects");
    std::fs::create_dir_all(f.root.join("outside-objects")).expect("outside");
    std::os::unix::fs::symlink(
        f.root.join("outside-objects"),
        f.repo_root.join(".git/objects"),
    )
    .expect("symlink");
    let error = detach_git_dir(&f.repo_root, &f.repo_root.join(".git")).expect_err("must fail");
    assert!(error.is_unavailable());
}

#[test]
fn nested_directory_form_git_metadata_is_sanitized_for_absolute_worktree_targets() {
    let f = repo();
    let inner = repo();
    copy_dir_all(&inner.repo_root, &f.repo_root.join("inner")).expect("copy");
    git(
        &f.repo_root.join("inner"),
        &["config", "core.worktree", &inner.repo_root.to_string_lossy()],
    )
    .expect("config");
    let result = scan_nested_git_dirs(&f.repo_root).expect("scan");
    assert!(result.nested_git_rewritten.is_empty());
    assert!(git(&f.repo_root.join("inner"), &["config", "--get", "core.worktree"]).is_err());
}

#[test]
fn deeply_nested_file_form_gitdir_pointers_are_still_rewritten() {
    let f = repo();
    let depth = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let deep = depth
        .iter()
        .fold(f.repo_root.clone(), |path, part| path.join(part));
    std::fs::create_dir_all(&deep).expect("deep");
    std::fs::create_dir_all(f.root.join("outside-admin")).expect("outside admin");
    let modules = depth
        .iter()
        .fold(f.repo_root.join(".git/modules"), |path, part| path.join(part));
    std::fs::create_dir_all(&modules).expect("modules");
    std::fs::write(
        deep.join(".git"),
        format!("gitdir: {}\n", f.root.join("outside-admin").display()),
    )
    .expect("gitfile");
    let result = scan_nested_git_dirs(&f.repo_root).expect("scan");
    assert!(result
        .nested_git_rewritten
        .contains(&depth.join("/")));
}

#[test]
fn admin_directories_outside_the_common_worktrees_area_are_never_deleted() {
    let f = repo();
    let merged = f.root.join("merged");
    std::fs::create_dir_all(&merged).expect("merged");
    let admin = f.root.join("planted-admin");
    std::fs::create_dir_all(&admin).expect("admin");
    std::fs::write(admin.join("gitdir"), format!("{}\n", merged.join(".git").display()))
        .expect("gitdir");
    std::fs::write(admin.join("HEAD"), "ref: refs/heads/main\n").expect("HEAD");
    std::fs::write(
        merged.join(".git"),
        format!("gitdir: {}\n", admin.display()),
    )
    .expect("gitfile");
    assert_eq!(
        detach_git_dir(&merged, &f.repo_root.join(".git")).expect("detach"),
        DetachOutcome::Detached
    );
    assert!(admin.exists());
    let _ = fixture();
}

struct WrappingBackend {
    inner: BackendRef,
    counter: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    corrupt_once: bool,
}

impl isolation_core::IsolationBackend for WrappingBackend {
    fn kind(&self) -> isolation_core::BackendKind {
        self.inner.kind()
    }

    fn clones_tree(&self) -> bool {
        self.inner.clones_tree()
    }

    fn probe(
        &self,
        repo_root: &Path,
        ctx: Option<&isolation_core::IsolationContext>,
    ) -> isolation_core::IsolationResult<isolation_core::ProbeResult> {
        self.inner.probe(repo_root, ctx)
    }

    fn start(
        &self,
        lower: &Path,
        merged: &Path,
        ctx: &isolation_core::IsolationContext,
    ) -> isolation_core::IsolationResult<Option<isolation_core::StartDetail>> {
        let detail = self.inner.start(lower, merged, ctx)?;
        let count = self
            .counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.corrupt_once && count == 0 {
            let admin = std::fs::read_to_string(merged.join(".git"))
                .expect("gitfile")
                .trim()
                .strip_prefix("gitdir: ")
                .expect("prefix")
                .to_string();
            std::fs::write(Path::new(&admin).join("index"), "broken").expect("corrupt index");
        }
        Ok(detail)
    }

    fn stop(&self, merged: &Path) -> isolation_core::IsolationResult<()> {
        self.inner.stop(merged)
    }
}
