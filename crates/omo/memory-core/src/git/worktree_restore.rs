use std::collections::BTreeSet;

use super::errors::GitError;
use super::repo::GitMemoryRepo;
use super::repo_arguments::normalize_pathspecs;

pub fn restore_worktree_from_head(
    repo: &GitMemoryRepo,
    paths: &[impl AsRef<str>],
) -> Result<(), GitError> {
    let normalized = normalize_pathspecs(paths);
    if normalized.is_empty() {
        return Ok(());
    }
    let at_head: BTreeSet<String> = repo.ls_tree(Some("HEAD"), None)?.into_iter().collect();
    for path in normalized {
        let target = repo.dir.join(&path);
        if at_head.contains(&path) {
            let content = repo.show("HEAD", &path)?;
            if let Some(parent) = target.parent() {
                crate::fs::create_dir_all(parent).map_err(GitError::Io)?;
            }
            crate::fs::write(&target, content.as_bytes()).map_err(GitError::Io)?;
        } else {
            match crate::fs::remove_file(&target) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(GitError::Io(error)),
            }
        }
    }
    Ok(())
}
