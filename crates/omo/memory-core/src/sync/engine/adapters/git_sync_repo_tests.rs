use super::*;

fn git(dir: &Path, argv: &[&str]) -> Result<String, Box<dyn std::error::Error>> {
    let out = std::process::Command::new("git")
        .args(argv)
        .current_dir(dir)
        .output()?;
    if !out.status.success() {
        return Err(format!("git {argv:?}: {}", String::from_utf8_lossy(&out.stderr)).into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn init(dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    git(dir, &["init", "-q"])?;
    git(dir, &["config", "user.email", "a@b.c"])?;
    git(dir, &["config", "user.name", "A"])?;
    Ok(())
}

fn repo(dir: &Path) -> GitSyncRepo {
    create_git_sync_repo(GitSyncRepoOptions {
        root: dir.to_path_buf(),
        exec: None,
    })
}

#[test]
fn commits_only_the_given_paths_as_omo_sync_and_finds_its_trailer()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    init(dir.path())?;
    std::fs::write(dir.path().join("a.md"), "a")?;
    std::fs::write(dir.path().join("other.md"), "dirty")?;
    let repo = repo(dir.path());
    assert_eq!(repo.head()?, None);
    assert!(!repo.has_sync_commit("1-1")?);
    repo.commit(
        &["a.md".to_string()],
        "sync: 1 memories updated\n\nSync-Seq: 1-1\nSync-Principals: x",
    )?;
    assert!(repo.has_sync_commit("1-1")?);
    assert!(!repo.has_sync_commit("2-2")?);
    assert_eq!(
        git(dir.path(), &["log", "-1", "--format=%an <%ae>"])?,
        "OmO Sync <sync@omo.invalid>"
    );
    assert_eq!(
        git(dir.path(), &["ls-tree", "-r", "--name-only", "HEAD"])?,
        "a.md"
    );
    assert!(git(dir.path(), &["status", "--porcelain"])?.contains("?? other.md"));
    Ok(())
}

#[test]
fn staging_a_never_tracked_missing_path_is_a_no_op() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    init(dir.path())?;
    std::fs::write(dir.path().join("keep.md"), "k")?;
    let repo = repo(dir.path());
    repo.commit(&["keep.md".to_string()], "seed")?;
    let tree = repo.stage_tree(&["ghost.md".to_string()])?;
    assert_eq!(tree, repo.head_tree()?);
    Ok(())
}

#[test]
fn a_missing_tracked_path_stages_as_a_delete_without_touching_the_shared_index()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    init(dir.path())?;
    std::fs::write(dir.path().join("a.md"), "a")?;
    let repo = repo(dir.path());
    repo.commit(&["a.md".to_string()], "seed")?;
    repo.sync_index(&["a.md".to_string()])?;
    std::fs::remove_file(dir.path().join("a.md"))?;
    let tree = repo.stage_tree(&["a.md".to_string()])?;
    assert_ne!(tree, repo.head_tree()?);
    assert_eq!(git(dir.path(), &["ls-files"])?, "a.md");
    Ok(())
}

#[test]
fn shared_index_is_reconciled_after_commit_or_resume() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    init(dir.path())?;
    std::fs::write(dir.path().join("a.md"), "first")?;
    std::fs::write(dir.path().join("other.md"), "first")?;
    let repo = repo(dir.path());
    repo.commit(&["a.md".into(), "other.md".into()], "seed")?;
    repo.sync_index(&["a.md".into(), "other.md".into()])?;
    std::fs::write(dir.path().join("a.md"), "synced")?;
    std::fs::write(dir.path().join("other.md"), "staged local")?;
    git(dir.path(), &["add", "other.md"])?;
    repo.commit(&["a.md".into()], "sync")?;
    repo.sync_index(&["a.md".into()])?;
    repo.sync_index(&["a.md".into()])?;
    assert_eq!(
        git(dir.path(), &["diff", "--cached", "--name-only"])?,
        "other.md"
    );
    assert_eq!(git(dir.path(), &["diff", "--name-only"])?, "");
    Ok(())
}
#[test]
fn unknown_lock_ownership_is_classified_and_kept() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    init(dir.path())?;
    std::fs::write(dir.path().join("a.md"), "a")?;
    let repo = repo(dir.path());
    repo.commit(&["a.md".into()], "seed")?;
    std::fs::write(dir.path().join(".git/index.lock"), "held")?;
    assert!(matches!(
        repo.sync_index(&["a.md".into()]),
        Err(super::super::super::error::EngineError::StrandedLock(_))
    ));
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".git/index.lock"))?,
        "held"
    );
    Ok(())
}
