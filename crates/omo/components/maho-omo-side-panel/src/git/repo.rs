use std::path::{Path, PathBuf};

pub fn find_git_root(cwd: &Path) -> Option<PathBuf> {
    let current = if cwd.is_absolute() { cwd.to_path_buf() } else { std::env::current_dir().ok()?.join(cwd) };
    current.ancestors().find(|path| path.join(".git").exists()).map(Path::to_path_buf)
}

pub fn read_git_branch(root: &Path) -> Option<String> {
    let candidate = root.join(".git");
    if !candidate.exists() { return None; }
    let git_dir = match std::fs::read_to_string(&candidate) {
        Ok(contents) => contents.trim().lines().find_map(|line| line.strip_prefix("gitdir:")
            .map(|target| root.join(target.trim()))).unwrap_or(candidate),
        Err(_) => candidate,
    };
    let contents = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = contents.trim();
    if head.is_empty() { return None; }
    if let Some(reference) = head.strip_prefix("ref:").map(str::trim) {
        if reference.is_empty() {
            return None;
        }
        return Some(
            reference
                .strip_prefix("refs/heads/")
                .unwrap_or(reference)
                .to_owned(),
        );
    }
    Some(head.chars().take(7).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbolic_non_branch_ref_does_not_return_ref_re() {
        let temp_dir = std::env::temp_dir().join(format!(
            "test_read_git_branch_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let git_dir = temp_dir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/tags/v1.0.0\n").unwrap();
        let branch = read_git_branch(&temp_dir);
        assert_ne!(branch, Some("ref: re".into()));
        assert_eq!(branch, Some("refs/tags/v1.0.0".into()));
        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn symbolic_branch_ref_returns_branch_name() {
        let temp_dir = std::env::temp_dir().join(format!(
            "test_read_git_branch_branch_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let git_dir = temp_dir.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(read_git_branch(&temp_dir), Some("main".into()));
        let _ = std::fs::remove_dir_all(&temp_dir);
    }
}
