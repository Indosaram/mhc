//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-run-local-repo.ts`.
//!
//! The one local repository shape a `run` url slot may name (S40 PR 2, design v2.10, `senpi#5645`).
//! The hand does not emulate git's repository lookup; it accepts a plain repository at an absolute,
//! already-normal path whose config holds only an exact key allowlist (design v3.1), and refuses
//! anything that could redirect git's reads or writes.

use std::path::{Path, PathBuf};

use maho_agent::execution::device_files::{DeviceFileError, DeviceFileErrorCode};
use tokio::process::Command;

const SHAPE: &str = "local_repo_shape_unsupported";
/// Entries the redirect walk inspects before it refuses instead of guessing; a memory mirror has a
/// few hundred refs.
const WALK_BOUND: usize = 20_000;

fn refuse(detail: impl std::fmt::Display) -> DeviceFileError {
    DeviceFileError::new(
        DeviceFileErrorCode::RefusedByPolicy,
        format!("{SHAPE}: {detail}"),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
    Link,
    Other,
}

async fn kind_of(path: &Path) -> Option<Kind> {
    let entry = tokio::fs::symlink_metadata(path).await.ok()?;
    let ft = entry.file_type();
    if ft.is_symlink() {
        Some(Kind::Link)
    } else if ft.is_file() {
        Some(Kind::File)
    } else if ft.is_dir() {
        Some(Kind::Dir)
    } else {
        Some(Kind::Other)
    }
}

/// Refuses a symlink at `path` or (for a directory) among its entries, recursively when asked,
/// within the shared budget.
async fn no_links(path: &Path, recursive: bool, budget: &mut isize) -> Result<(), DeviceFileError> {
    let mut stack: Vec<(PathBuf, bool)> = vec![(path.to_path_buf(), true)];
    while let Some((current, is_root)) = stack.pop() {
        let Some(kind) = kind_of(&current).await else {
            continue;
        };
        if kind == Kind::Link {
            return Err(refuse(format!("symlink at {}", current.display())));
        }
        if kind != Kind::Dir || (!is_root && !recursive) {
            continue;
        }
        let mut rd = tokio::fs::read_dir(&current)
            .await
            .map_err(|e| refuse(e.to_string()))?;
        while let Some(entry) = rd.next_entry().await.map_err(|e| refuse(e.to_string()))? {
            *budget -= 1;
            if *budget < 0 {
                return Err(refuse(format!(
                    "more than {WALK_BOUND} entries under the repository"
                )));
            }
            let child = entry.path();
            let child_kind = kind_of(&child).await;
            if child_kind == Some(Kind::Link) {
                return Err(refuse(format!("symlink at {}", child.display())));
            }
            if recursive && child_kind == Some(Kind::Dir) {
                stack.push((child, false));
            }
        }
    }
    Ok(())
}

/// The git dir: `HEAD` a regular file, `objects/` and `refs/` real directories, no `commondir` or
/// `alternates`, and no symlink anywhere git would follow it.
async fn plain_git_dir(dir: &Path) -> Result<(), DeviceFileError> {
    let head = kind_of(&dir.join("HEAD")).await;
    let objects = kind_of(&dir.join("objects")).await;
    let refs = kind_of(&dir.join("refs")).await;
    let commondir = kind_of(&dir.join("commondir")).await;
    let alternates = kind_of(&dir.join("objects/info/alternates")).await;

    if head != Some(Kind::File) || objects != Some(Kind::Dir) || refs != Some(Kind::Dir) {
        return Err(refuse(format!(
            "{} is not a plain git directory",
            dir.display()
        )));
    }
    if commondir.is_some() || alternates.is_some() {
        return Err(refuse(format!(
            "{} redirects through commondir or alternates",
            dir.display()
        )));
    }
    let mut budget = WALK_BOUND as isize;
    for entry in ["HEAD", "config", "packed-refs"] {
        no_links(&dir.join(entry), false, &mut budget).await?;
    }
    no_links(&dir.join("refs"), true, &mut budget).await?;
    no_links(&dir.join("hooks"), true, &mut budget).await?;
    no_links(&dir.join("logs"), true, &mut budget).await?;
    no_links(&dir.join("worktrees"), false, &mut budget).await?;
    no_links(&dir.join("objects"), false, &mut budget).await?;
    for sub in ["info", "pack"] {
        no_links(&dir.join("objects").join(sub), false, &mut budget).await?;
    }
    Ok(())
}

/// `git config --list` of one file, includes NOT followed, under a fixed environment the caller
/// cannot shape.
async fn git_config(file: &Path) -> Result<Vec<(String, String)>, DeviceFileError> {
    let path_env = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".to_owned());
    let mut cmd = Command::new("git");
    cmd.arg("config")
        .arg("--file")
        .arg(file)
        .arg("--no-includes")
        .arg("--list")
        .arg("-z")
        .env_clear()
        .env("PATH", path_env)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LANG", "C")
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| refuse("target config is unreadable"))?
        .map_err(|_| refuse("target config is unreadable"))?;
    if !output.status.success() {
        return Err(refuse("target config is unreadable"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut entries = Vec::new();
    for record in text.split('\0').filter(|s| !s.is_empty()) {
        match record.split_once('\n') {
            Some((k, v)) => entries.push((k.to_owned(), v.to_owned())),
            None => entries.push((record.to_owned(), String::new())),
        }
    }
    Ok(entries)
}

fn is_init_key(key: &str) -> bool {
    matches!(
        key,
        "core.repositoryformatversion"
            | "core.filemode"
            | "core.bare"
            | "core.ignorecase"
            | "core.precomposeunicode"
            | "core.logallrefupdates"
    )
}

fn is_clone_key(key: &str) -> bool {
    let Some(rest) = key.strip_prefix("remote.") else {
        return false;
    };
    for suffix in [".url", ".fetch", ".mirror", ".tagopt"] {
        if let Some(mid) = rest.strip_suffix(suffix)
            && !mid.is_empty()
        {
            return true;
        }
    }
    false
}

fn is_working_clone_key(key: &str) -> bool {
    if key == "user.name" || key == "user.email" {
        return true;
    }
    if let Some(rest) = key.strip_prefix("branch.") {
        for suffix in [".remote", ".merge"] {
            if let Some(mid) = rest.strip_suffix(suffix)
                && !mid.is_empty()
            {
                return true;
            }
        }
    }
    false
}

/// The target's config holds ONLY the keys `git init` and `git clone` write (design v3.1, an
/// allowlist, never a denylist): hooks, filters, `receive.*`, `extensions.*`, includes and every
/// other execution or redirect key are refused by construction.
async fn allowlisted_config(dir: &Path, bare: bool) -> Result<(), DeviceFileError> {
    let config = dir.join("config");
    if kind_of(&config).await != Some(Kind::File) {
        return Err(refuse(format!("{} has no config file", dir.display())));
    }
    let entries = git_config(&config).await?;
    for (key, value) in entries {
        if key == "core.bare" {
            let expected = if bare { "true" } else { "false" };
            if value != expected {
                return Err(refuse(format!(
                    "core.bare={value} on a {} target",
                    if bare { "bare" } else { "working-clone" }
                )));
            }
            continue;
        }
        let allowed =
            is_init_key(&key) || is_clone_key(&key) || (!bare && is_working_clone_key(&key));
        if !allowed {
            return Err(refuse(format!(
                "target config key {key} is not allowlisted"
            )));
        }
    }
    Ok(())
}

/// The git dir an absolute, normal local path names, in strict canonical shape.
pub async fn canonical_local_repo(path: &Path) -> Result<PathBuf, DeviceFileError> {
    let Some(parent_in) = path.parent() else {
        return Err(refuse(format!("no repository at {}", path.display())));
    };
    let Some(base) = path.file_name() else {
        return Err(refuse(format!("no repository at {}", path.display())));
    };
    let parent = tokio::fs::canonicalize(parent_in)
        .await
        .map_err(|_| refuse(format!("no repository at {}", path.display())))?;
    let real = parent.join(base);
    let self_kind = kind_of(&real).await;
    if self_kind == Some(Kind::Link) {
        return Err(refuse(format!("{} is a symlink", path.display())));
    }
    if self_kind != Some(Kind::Dir) {
        return Err(refuse(format!("no repository at {}", path.display())));
    }
    let mut sibling_git = real.as_os_str().to_os_string();
    sibling_git.push(".git");
    if kind_of(Path::new(&sibling_git)).await.is_some() {
        return Err(refuse(format!("{}.git exists", path.display())));
    }
    let dot_git = kind_of(&real.join(".git")).await;
    if dot_git.is_some() && dot_git != Some(Kind::Dir) {
        return Err(refuse(format!(
            "{}/.git is not a directory",
            path.display()
        )));
    }
    let git_dir = if dot_git == Some(Kind::Dir) {
        real.join(".git")
    } else {
        real
    };
    plain_git_dir(&git_dir).await?;
    allowlisted_config(&git_dir, dot_git != Some(Kind::Dir)).await?;
    Ok(git_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_key_allowlist_matches_only_init_and_clone_keys() {
        assert!(is_init_key("core.bare"));
        assert!(is_clone_key("remote.origin.url"));
        assert!(is_clone_key("remote.origin.fetch"));
        assert!(!is_clone_key("remote..url"));
        assert!(!is_clone_key("remote.origin.proxy"));
        assert!(is_working_clone_key("branch.main.remote"));
        assert!(is_working_clone_key("user.name"));
        assert!(!is_working_clone_key("core.hooksPath"));
    }

    #[tokio::test]
    async fn accepts_plain_bare_repo_and_rejects_disallowed_config_or_alternates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canon");
        let repo = root.join("bare.git");
        std::fs::create_dir_all(repo.join("objects/info")).expect("objects");
        std::fs::create_dir_all(repo.join("refs")).expect("refs");
        std::fs::write(repo.join("HEAD"), "ref: refs/heads/main\n").expect("head");
        std::fs::write(
            repo.join("config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = true\n",
        )
        .expect("config");

        let resolved = canonical_local_repo(&repo).await.expect("plain bare repo");
        assert_eq!(resolved, repo);

        // Adding alternates fails.
        std::fs::write(repo.join("objects/info/alternates"), "/tmp/other\n").expect("alt");
        let err = canonical_local_repo(&repo).await.expect_err("alternates");
        assert_eq!(err.code, DeviceFileErrorCode::RefusedByPolicy);
        assert!(err.message.contains(SHAPE));
        std::fs::remove_file(repo.join("objects/info/alternates")).expect("rm alt");

        // Adding hooksPath to config fails.
        std::fs::write(
            repo.join("config"),
            "[core]\n\trepositoryformatversion = 0\n\tbare = true\n\thooksPath = /tmp/evil\n",
        )
        .expect("bad config");
        let err = canonical_local_repo(&repo).await.expect_err("bad config");
        assert_eq!(err.code, DeviceFileErrorCode::RefusedByPolicy);
    }
}
