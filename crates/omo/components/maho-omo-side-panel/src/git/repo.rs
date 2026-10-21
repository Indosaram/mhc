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
    if let Some(branch) = head.strip_prefix("ref:").map(str::trim_start)
        .and_then(|reference| reference.strip_prefix("refs/heads/"))
        .filter(|branch| !branch.is_empty()) {
        return Some(branch.into());
    }
    Some(head.chars().take(7).collect())
}
