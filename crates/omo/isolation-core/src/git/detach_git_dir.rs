use std::path::{Path, PathBuf};

use crate::backend::{IsolationError, Result};
use crate::git::command::{exists, git, git_result, str_args};
use crate::util::{dirname, normalize_lexically, relative_path, symlink_at};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetachOutcome {
    NoGit,
    Independent,
    Detached,
}

impl DetachOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            DetachOutcome::NoGit => "no-git",
            DetachOutcome::Independent => "independent",
            DetachOutcome::Detached => "detached",
        }
    }
}

fn canonical(path: &Path) -> Result<PathBuf> {
    // The native resolver expands 8.3 short names (RUNNER~1) that git's own
    // long-path registrations carry; a JS resolver leaves them unexpanded, so
    // back-pointer identity checks would never match on such paths.
    let value = std::fs::canonicalize(path)?;
    if cfg!(windows) {
        Ok(PathBuf::from(value.to_string_lossy().to_lowercase()))
    } else {
        Ok(value)
    }
}

fn inside(real: &Path, path: &Path) -> bool {
    path == real || path.starts_with(real)
}

fn gitdir(entry: &Path) -> Result<PathBuf> {
    let text = std::fs::read_to_string(entry)?;
    let text = text.trim();
    let Some(rest) = text.strip_prefix("gitdir: ") else {
        return Err(IsolationError::other(format!(
            "Invalid gitdir file: {}",
            entry.display()
        )));
    };
    Ok(normalize_lexically(&dirname(entry).join(rest)))
}

fn remove_locks(root: &Path) -> Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().ends_with(".lock") {
            remove_path(&path)?;
        } else if entry.file_type()?.is_dir() {
            remove_locks(&path)?;
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(path)?,
        Ok(_) => std::fs::remove_file(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn unset(config: &Path, key: &str) -> Result<()> {
    let result = git_result(
        &dirname(config),
        &str_args(&[
            "config",
            "--file",
            &config.to_string_lossy(),
            "--unset-all",
            key,
        ]),
        None,
    )?;
    if result.code != 0 && result.code != 5 {
        return Err(IsolationError::other(format!(
            "git config unset {key}: {}",
            result.stderr
        )));
    }
    Ok(())
}

fn copy_recursive_no_follow(source: &Path, destination: &Path) -> Result<()> {
    let info = std::fs::symlink_metadata(source)?;
    if info.file_type().is_symlink() {
        let target = std::fs::read_link(source)?;
        symlink_at(&target, destination, source)?;
        return Ok(());
    }
    if info.is_dir() {
        std::fs::create_dir(destination)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_recursive_no_follow(&entry.path(), &destination.join(entry.file_name()))?;
        }
        return Ok(());
    }
    let mut source_file = std::fs::File::open(source)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut destination_file = options.open(destination)?;
    std::io::copy(&mut source_file, &mut destination_file)?;
    Ok(())
}

fn copy_optional(source: &Path, destination: &Path) -> Result<()> {
    if !exists(source)? {
        return Ok(());
    }
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    copy_recursive_no_follow(source, destination)
}

fn walk_metadata(dir: &Path, real: &Path, name: &str, git_dir: &Path) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            if !inside(real, &canonical(&path)?) {
                return Err(IsolationError::unavailable(format!(
                    "git metadata under {name} escapes the repository copy: {}",
                    git_dir.display()
                )));
            }
        } else if file_type.is_dir() {
            walk_metadata(&path, real, name, git_dir)?;
        }
    }
    Ok(())
}

/// Shared Git mutation targets (objects, refs, HEAD, index, config) must stay
/// inside the copied metadata directory; a symlink escape would let isolated
/// operations mutate the source repository's store.
fn assert_contained_metadata(git_dir: &Path) -> Result<()> {
    let real = canonical(git_dir)?;
    for name in ["objects", "refs", "HEAD", "config", "index"] {
        let area = git_dir.join(name);
        if !exists(&area)? {
            continue;
        }
        let info = std::fs::symlink_metadata(&area)?;
        if info.file_type().is_symlink() {
            if !inside(&real, &canonical(&area)?) {
                return Err(IsolationError::unavailable(format!(
                    "git metadata {name} escapes the repository copy: {}",
                    git_dir.display()
                )));
            }
            continue;
        }
        if !info.is_dir() {
            continue;
        }
        walk_metadata(&area, &real, name, git_dir)?;
    }
    Ok(())
}

/// Directory-form Git metadata becomes standalone: no locks, no external worktree
/// target, and no shared mutation targets.
fn sanitize_standalone_git_dir(entry: &Path) -> Result<()> {
    remove_locks(entry)?;
    let config = entry.join("config");
    if exists(&config)? {
        unset(&config, "core.worktree")?;
        unset(&config, "core.bare")?;
    }
    assert_contained_metadata(entry)
}

fn is_allowlisted_config_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("user.") {
        return !rest.is_empty();
    }
    let Some(rest) = lower.strip_prefix("core.") else {
        return false;
    };
    matches!(
        rest,
        "filemode" | "splitindex" | "symlinks" | "autocrlf" | "ignorecase"
    ) || rest.starts_with("sparsecheckout")
}

pub fn detach_git_dir(worktree_root: &Path, source_common_dir: &Path) -> Result<DetachOutcome> {
    let entry = worktree_root.join(".git");
    if !exists(&entry)? {
        return Ok(DetachOutcome::NoGit);
    }
    let meta = std::fs::symlink_metadata(&entry)?;
    if meta.is_dir() {
        sanitize_standalone_git_dir(&entry)?;
        return Ok(DetachOutcome::Independent);
    }
    if !meta.is_file() {
        return Err(IsolationError::unavailable(
            ".git must not share metadata through a symlink",
        ));
    }
    let admin = gitdir(&entry)?;
    // Canonicalize through the same resolver and case rules as every identity
    // comparison below, or the prune guard compares mismatched forms on win32.
    let common = canonical(source_common_dir)?;
    let mut own_admin = false;
    let admin_gitdir = admin.join("gitdir");
    if exists(&admin_gitdir)? {
        let back_pointer = std::fs::read_to_string(&admin_gitdir)?;
        let target = normalize_lexically(&admin.join(back_pointer.trim()));
        if exists(&target)? {
            own_admin = canonical(&target)? == canonical(&entry)?;
        }
    }
    let private_dir = worktree_root.join(".git-private");
    std::fs::create_dir(&private_dir)?;
    let result = (|| -> Result<()> {
        std::fs::create_dir_all(private_dir.join("objects/info"))?;
        std::fs::create_dir(private_dir.join("refs"))?;
        std::fs::write(private_dir.join("HEAD"), std::fs::read(admin.join("HEAD"))?)?;
        std::fs::write(
            private_dir.join("config"),
            "[core]\n\trepositoryformatversion = 0\n",
        )?;
        let common_config = common.join("config");
        if exists(&common_config)? {
            let config = git(
                &common,
                &str_args(&[
                    "config",
                    "--file",
                    &common_config.to_string_lossy(),
                    "--null",
                    "--list",
                    "--no-includes",
                ]),
                None,
            )?;
            let config = String::from_utf8_lossy(&config).into_owned();
            for record in config.split('\0').filter(|record| !record.is_empty()) {
                let (key, value) = match record.find('\n') {
                    Some(index) => (&record[..index], &record[index + 1..]),
                    None => (record, "true"),
                };
                if is_allowlisted_config_key(key) {
                    git(
                        &private_dir,
                        &str_args(&[
                            "config",
                            "--file",
                            &private_dir.join("config").to_string_lossy(),
                            "--add",
                            key,
                            value,
                        ]),
                        None,
                    )?;
                }
            }
        }
        let common_refs = common.join("refs");
        for name in std::fs::read_dir(&common_refs)? {
            let name = name?.file_name();
            copy_optional(
                &common_refs.join(&name),
                &private_dir.join("refs").join(&name),
            )?;
        }
        copy_optional(&common.join("packed-refs"), &private_dir.join("packed-refs"))?;
        let mut names: Vec<String> = vec![
            "index".to_string(),
            "info/sparse-checkout".to_string(),
            "shallow".to_string(),
        ];
        for entry in std::fs::read_dir(&admin)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.starts_with("sharedindex.") {
                names.push(name);
            }
        }
        for name in names {
            copy_optional(&admin.join(&name), &private_dir.join(&name))?;
        }
        if !exists(&private_dir.join("shallow"))? {
            copy_optional(&common.join("shallow"), &private_dir.join("shallow"))?;
        }
        std::fs::write(
            private_dir.join("objects/info/alternates"),
            format!("{}\n", common.join("objects").display()),
        )?;
        remove_locks(&private_dir)?;
        remove_path(&entry)?;
        std::fs::rename(&private_dir, &entry)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_dir_all(&private_dir);
        return Err(error);
    }
    if own_admin {
        // A matching back-pointer alone is not proof of ownership: the registration
        // must also live under the source common directory's worktrees area.
        let worktrees_root = common.join("worktrees");
        let admin_canonical = canonical(&admin)?;
        if inside(&worktrees_root, &admin_canonical) {
            std::fs::remove_dir_all(&admin)?;
            git(
                &common,
                &str_args(&["--git-dir", &common.to_string_lossy(), "worktree", "prune"]),
                None,
            )?;
        }
    }
    Ok(DetachOutcome::Detached)
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NestedGitResult {
    pub nested_git_rewritten: Vec<String>,
    pub nested_git_skipped: Vec<String>,
}

pub fn scan_nested_git_dirs(merged: &Path) -> Result<NestedGitResult> {
    let mut result = NestedGitResult::default();
    let root = canonical(merged)?;
    fn walk(dir: &Path, rel: &Path, depth: usize, root: &Path, merged: &Path, result: &mut NestedGitResult) -> Result<()> {
        if depth > 0 {
            let entry = dir.join(".git");
            if exists(&entry)? {
                let meta = std::fs::symlink_metadata(&entry)?;
                if meta.file_type().is_symlink() {
                    return Err(IsolationError::unavailable(format!(
                        "submodule {} shares the source gitdir",
                        rel.display()
                    )));
                }
                if meta.is_dir() {
                    sanitize_standalone_git_dir(&entry)?;
                }
                if meta.is_file() {
                    let target = gitdir(&entry)?;
                    if !inside(root, &canonical(&target)?) {
                        let path = rel.to_string_lossy().replace('\\', "/");
                        let replacement = merged.join(".git").join("modules").join(rel);
                        if !exists(&replacement)? || !inside(root, &canonical(&replacement)?) {
                            return Err(IsolationError::unavailable(format!(
                                "submodule {path} shares the source gitdir"
                            )));
                        }
                        let relative = relative_path(dir, &replacement)
                            .to_string_lossy()
                            .replace('\\', "/");
                        std::fs::write(&entry, format!("gitdir: {relative}\n"))?;
                        result.nested_git_rewritten.push(path);
                    }
                }
            }
        }
        // No fixed depth cap: symlinked directories are not descended (readdir reports
        // the link itself), so the walk cannot cycle, and stopping early would leave
        // deeper external Git-directory pointers unchecked.
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type()?.is_dir() && name != ".git" && name != "node_modules" {
                walk(&entry.path(), &rel.join(&name), depth + 1, root, merged, result)?;
            }
        }
        Ok(())
    }
    walk(merged, Path::new(""), 0, &root, merged, &mut result)?;
    Ok(result)
}
