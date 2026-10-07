use std::path::Path;

use isolation_core::backends::git_fixture::{git, repo};
use isolation_core::test_support::fixture;
use isolation_core::{
    capture_baseline, merge_isolated_changes, nested_path, write_artifacts, ArtifactOptions,
    DeltaPatchResult, IsolationMergeOptions, MergeMode, ISOLATION_BASELINE_MAX_CONTENT_BYTES,
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
fn nested_path_rejects_paths_that_escape_the_root_through_symlinked_components() {
    let f = fixture();
    let root = f.root.join("root");
    std::fs::create_dir_all(root.join("nested")).expect("nested");
    std::os::unix::fs::symlink(&f.root, root.join("nested/escape")).expect("symlink");
    assert!(nested_path(&root, "nested/escape/repo").is_err());
    assert_eq!(
        nested_path(&root, "nested/plain").expect("plain"),
        root.join("nested/plain")
    );
}

#[test]
fn write_artifacts_refuses_artifact_destinations_redirected_by_a_pre_existing_symlink() {
    let f = fixture();
    let artifacts_dir = f.root.join("artifacts");
    std::fs::create_dir_all(artifacts_dir.join("isolation")).expect("isolation dir");
    std::os::unix::fs::symlink(&f.root, artifacts_dir.join("isolation/test")).expect("symlink");
    let delta = DeltaPatchResult {
        root_patch: "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n".to_string(),
        nested_patches: Vec::new(),
    };
    let result = write_artifacts(
        &delta,
        &ArtifactOptions {
            id: "test".to_string(),
            artifacts_dir,
            lock_hook: None,
        },
    );
    assert!(result.is_err());
}

#[test]
fn nested_merge_refuses_a_nested_repository_path_replaced_by_an_external_symlink() {
    let f = repo();
    let inner = repo();
    copy_dir_all(&inner.repo_root, &f.repo_root.join("inner")).expect("nested copy");
    let baseline = capture_baseline(&f.repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES).expect("baseline");
    let isolation_dir = f.root.join("child");
    copy_dir_all(&f.repo_root, &isolation_dir).expect("isolation copy");
    std::fs::write(isolation_dir.join("inner/tracked"), "child edit\n").expect("edit");
    // After baseline capture, the source's nested repository is replaced by a symlink out.
    std::fs::remove_dir_all(f.repo_root.join("inner")).expect("remove nested");
    std::os::unix::fs::symlink(&f.root, f.repo_root.join("inner")).expect("symlink");
    let result = merge_isolated_changes(IsolationMergeOptions {
        id: "escape".to_string(),
        artifacts_dir: f.root.join("artifacts"),
        lock_hook: None,
        commit_message: None,
        mode: MergeMode::Patch,
        apply: true,
        repo_root: f.repo_root.clone(),
        isolation_dir,
        baseline,
        delta: None,
    });
    assert!(result.is_err());
    let _ = git(&f.repo_root, &["status", "--porcelain"]);
}
