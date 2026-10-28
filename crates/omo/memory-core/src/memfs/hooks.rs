//! Git hook scripts installation into memory repositories.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::memfs::hooks_commit_msg::{COMMIT_MSG_HOOK_SCRIPT, with_memory_hook_checker};
use crate::memfs::hooks_scripts::{POST_COMMIT_HOOK_SCRIPT, PRE_COMMIT_HOOK_SCRIPT};

pub use crate::memfs::hooks_commit_msg::MemoryHookChecker;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// Representation of an installed git hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledHook {
    pub name: String,
    pub path: PathBuf,
}

/// Optional configuration when installing memory repository hooks.
#[derive(Debug, Default, Clone)]
pub struct InstallHooksOptions {
    pub checker: Option<MemoryHookChecker>,
}

/// Write the memory hooks into `repo_path`, overwriting any previous copy.
pub fn install_hooks(repo_path: &Path) -> io::Result<Vec<InstalledHook>> {
    install_hooks_with_options(repo_path, &InstallHooksOptions::default())
}

/// Write the memory hooks into `repo_path` with the specified options.
pub fn install_hooks_with_options(
    repo_path: &Path,
    options: &InstallHooksOptions,
) -> io::Result<Vec<InstalledHook>> {
    let hooks_dir = resolve_hooks_dir(repo_path);
    fs::create_dir_all(&hooks_dir)?;

    let pre_commit_script =
        with_memory_hook_checker(PRE_COMMIT_HOOK_SCRIPT, "pre-commit", options.checker.as_ref());
    let commit_msg_script =
        with_memory_hook_checker(COMMIT_MSG_HOOK_SCRIPT, "commit-msg", options.checker.as_ref());

    let hooks = [
        ("pre-commit", pre_commit_script),
        ("commit-msg", commit_msg_script),
        ("post-commit", POST_COMMIT_HOOK_SCRIPT.to_string()),
    ];

    let mut installed = Vec::with_capacity(hooks.len());
    for (name, script) in hooks {
        let dest = hooks_dir.join(name);
        atomic_write_executable(&dest, script.as_bytes())?;
        installed.push(InstalledHook {
            name: name.to_string(),
            path: dest,
        });
    }

    Ok(installed)
}

/// Resolve the hooks directory for a repository or linked worktree.
pub fn resolve_hooks_dir(repo_path: &Path) -> PathBuf {
    resolve_common_git_dir(repo_path).join("hooks")
}

/// Resolve the common `.git` directory for normal repositories or linked worktrees.
pub fn resolve_common_git_dir(repo_path: &Path) -> PathBuf {
    let dot_git = repo_path.join(".git");
    if !dot_git.exists() || dot_git.is_dir() {
        return dot_git;
    }

    let pointer = match fs::read_to_string(&dot_git) {
        Ok(content) => content.trim().to_string(),
        Err(_) => return dot_git,
    };

    let git_dir_rel = if let Some(stripped) = pointer.strip_prefix("gitdir:") {
        stripped.trim()
    } else {
        return dot_git;
    };

    let git_dir = absolutize(Path::new(git_dir_rel), repo_path);
    let common_dir_file = git_dir.join("commondir");
    if !common_dir_file.exists() {
        return git_dir;
    }

    let common_dir_rel = match fs::read_to_string(&common_dir_file) {
        Ok(content) => content.trim().to_string(),
        Err(_) => return git_dir,
    };

    absolutize(Path::new(&common_dir_rel), &git_dir)
}

fn absolutize(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        crate::support::paths::resolve_from(base, path)
    }
}

fn atomic_write_executable(path: &Path, content: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "hook".to_string());
    let tmp_path = parent.join(format!(".{file_name}.tmp.{}", std::process::id()));

    fs::write(&tmp_path, content)?;

    #[cfg(unix)]
    {
        fs::set_permissions(&tmp_path, fs::Permissions::from_mode(0o755))?;
    }

    if fs::rename(&tmp_path, path).is_err() {
        let _ = fs::remove_file(path);
        fs::rename(&tmp_path, path)?;
    }

    Ok(())
}

#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
