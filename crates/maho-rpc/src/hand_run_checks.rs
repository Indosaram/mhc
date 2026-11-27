//! Port of senpi `packages/coding-agent/src/modes/rpc/hand-run-checks.ts`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use maho_agent::execution::device_files::{DeviceFileError, DeviceFileErrorCode};
use maho_tools::filesystem_policy::canonicalize_filesystem_path;
use tokio::process::Command;

use crate::hand_run_grammar::{TypedPath, TypedPathKind, local_url_path};
use crate::hand_run_local_repo::canonical_local_repo;

const MIN_GIT: (u32, u32) = (2, 29);

async fn git_output(
    argv: &[&str],
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<String, (Option<i32>, String)> {
    let mut cmd = Command::new("git");
    cmd.args(argv)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| (None, "git timed out".to_owned()))?
        .map_err(|e| (None, e.to_string()))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err((
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ))
    }
}

pub fn parse_git_version_supported(output: &str) -> bool {
    let Some(idx) = output.find("git version ") else {
        return false;
    };
    let rest = &output[idx + "git version ".len()..];
    let mut nums = rest
        .split(|c: char| !c.is_ascii_digit())
        .filter(|s| !s.is_empty());
    let (Some(maj_s), Some(min_s)) = (nums.next(), nums.next()) else {
        return false;
    };
    let (Ok(major), Ok(minor)) = (maj_s.parse::<u32>(), min_s.parse::<u32>()) else {
        return false;
    };
    major > MIN_GIT.0 || (major == MIN_GIT.0 && minor >= MIN_GIT.1)
}

static VERSION_CHECKED: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();

/// The hand's git is at least 2.29 (every subcommand in the allowlist is then a builtin, never an
/// alias or a PATH lookup).
pub async fn git_is_supported(env: &HashMap<String, String>) -> bool {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    *VERSION_CHECKED
        .get_or_init(|| async {
            match git_output(&["--version"], &cwd, env).await {
                Ok(out) => parse_git_version_supported(&out),
                Err(_) => false,
            }
        })
        .await
}

/// The run's working directory: resolved against the scope root, canonical, and an existing
/// directory.
pub async fn run_directory(root: &Path, cwd: &str) -> Result<PathBuf, DeviceFileError> {
    let joined = root.join(cwd);
    let canonical = canonicalize_filesystem_path(&joined)
        .await
        .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?;
    let entry = tokio::fs::metadata(&canonical)
        .await
        .map_err(|_| DeviceFileError::new(DeviceFileErrorCode::NotFound, cwd.to_owned()))?;
    if !entry.is_dir() {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::NotAFile,
            cwd.to_owned(),
        ));
    }
    Ok(canonical)
}

fn inside(path: &Path, dir: &Path) -> bool {
    path == dir || path.starts_with(dir)
}

async fn git_dirs(
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<Vec<PathBuf>, DeviceFileError> {
    let output = git_output(
        &["rev-parse", "--absolute-git-dir", "--git-common-dir"],
        cwd,
        env,
    )
    .await
    .map_err(|_| {
        DeviceFileError::new(DeviceFileErrorCode::RefusedByPolicy, "no repository at cwd")
    })?;
    let mut lines = output.trim().lines();
    let (Some(git_dir), Some(common_dir)) = (lines.next(), lines.next()) else {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            "no repository at cwd",
        ));
    };
    let c1 = canonicalize_filesystem_path(Path::new(git_dir))
        .await
        .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?;
    let c2 = canonicalize_filesystem_path(&cwd.join(common_dir))
        .await
        .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?;
    Ok(vec![c1, c2])
}

/// `GIT_INDEX_FILE` must name a file inside the repository's own git directory (or the common one
/// of a linked worktree).
pub async fn confined_index_file(
    value: &str,
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<PathBuf, DeviceFileError> {
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            "GIT_INDEX_FILE must be absolute",
        ));
    }
    let dirs = git_dirs(cwd, env).await?;
    let Some(parent_in) = path.parent() else {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            "GIT_INDEX_FILE is outside the repository's git directory",
        ));
    };
    let Some(base) = path.file_name() else {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            "GIT_INDEX_FILE is outside the repository's git directory",
        ));
    };
    let parent = canonicalize_filesystem_path(parent_in)
        .await
        .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?;
    if !dirs.iter().any(|dir| inside(&parent, dir)) {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            "GIT_INDEX_FILE is outside the repository's git directory",
        ));
    }
    Ok(parent.join(base))
}

/// Any `url.*` key (`insteadOf`, `pushInsteadOf`) in the run's effective config: git could rewrite
/// a judged url (V5).
pub async fn has_url_rewrites(
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<bool, DeviceFileError> {
    match git_output(&["config", "--get-regexp", "^url\\."], cwd, env).await {
        Ok(out) => Ok(!out.trim().is_empty()),
        Err((Some(1), _)) => Ok(false),
        Err((_, msg)) => Err(DeviceFileError::new(DeviceFileErrorCode::Io, msg)),
    }
}

/// The canonical location of `raw` (resolved against `cwd`); a symlink as the LAST component is
/// refused, never followed.
async fn canonical_target(raw: &str, cwd: &Path) -> Result<PathBuf, DeviceFileError> {
    let target = cwd.join(raw);
    if let Ok(meta) = tokio::fs::symlink_metadata(&target).await
        && meta.file_type().is_symlink()
    {
        return Err(DeviceFileError::new(
            DeviceFileErrorCode::RefusedByPolicy,
            format!("symlink: {raw}"),
        ));
    }
    let Some(parent_in) = target.parent() else {
        return Ok(target);
    };
    let Some(base) = target.file_name() else {
        return Ok(target);
    };
    let parent = canonicalize_filesystem_path(parent_in)
        .await
        .map_err(|e| DeviceFileError::new(DeviceFileErrorCode::Io, e.to_string()))?;
    Ok(parent.join(base))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOperation {
    Read,
    Write,
}

/// A filesystem location a planned git call reads or writes outside the confinement rules, for the
/// hand's policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyTarget {
    pub operation: PolicyOperation,
    pub canonical_path: PathBuf,
}

/// Confines the typed path arguments of a planned git call (v2.6/v2.7).
pub async fn confine_paths(
    paths: &[TypedPath],
    cwd: &Path,
    env: &HashMap<String, String>,
) -> Result<Vec<PolicyTarget>, DeviceFileError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let dirs = git_dirs(cwd, env).await?;
    let top = match git_output(&["rev-parse", "--show-toplevel"], cwd, env).await {
        Ok(out) => canonicalize_filesystem_path(Path::new(out.trim()))
            .await
            .ok(),
        Err(_) => None,
    };
    let mut targets = Vec::new();
    for typed in paths {
        if matches!(typed.kind, TypedPathKind::Source | TypedPathKind::Target) {
            let Some(local) = local_url_path(&typed.value) else {
                return Err(DeviceFileError::new(
                    DeviceFileErrorCode::RefusedByPolicy,
                    format!(
                        "local_repo_shape_unsupported: only an absolute file:// url: {}",
                        typed.value
                    ),
                ));
            };
            let operation = if typed.kind == TypedPathKind::Source {
                PolicyOperation::Read
            } else {
                PolicyOperation::Write
            };
            targets.push(PolicyTarget {
                operation,
                canonical_path: canonical_local_repo(Path::new(&local)).await?,
            });
            continue;
        }
        let canonical = canonical_target(&typed.value, cwd).await?;
        let in_git_dir = dirs.iter().any(|dir| {
            inside(&canonical, dir)
                || (typed.kind == TypedPathKind::Worktree && inside(dir, &canonical))
        });
        if typed.kind == TypedPathKind::Worktree {
            if in_git_dir {
                return Err(DeviceFileError::new(
                    DeviceFileErrorCode::RefusedByPolicy,
                    "worktree path is inside the repository's git directory",
                ));
            }
            targets.push(PolicyTarget {
                operation: PolicyOperation::Write,
                canonical_path: canonical,
            });
        } else if !in_git_dir && !top.as_ref().is_some_and(|t| inside(&canonical, t)) {
            return Err(DeviceFileError::new(
                DeviceFileErrorCode::RefusedByPolicy,
                format!("path outside the repository: {}", typed.value),
            ));
        }
    }
    Ok(targets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_git_version_floor_2_29() {
        assert!(parse_git_version_supported("git version 2.29.0"));
        assert!(parse_git_version_supported(
            "git version 2.43.1 (Apple Git-146)"
        ));
        assert!(parse_git_version_supported("git version 3.0.0"));
        assert!(!parse_git_version_supported("git version 2.28.9"));
        assert!(!parse_git_version_supported("not git"));
    }

    #[tokio::test]
    async fn run_directory_checks_existence_and_dir_type() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "x").expect("write");

        let ok = run_directory(dir.path(), ".").await.expect("ok dir");
        assert!(ok.is_dir());

        let missing = run_directory(dir.path(), "missing")
            .await
            .expect_err("missing");
        assert_eq!(missing.code, DeviceFileErrorCode::NotFound);

        let not_dir = run_directory(dir.path(), "file.txt")
            .await
            .expect_err("file");
        assert_eq!(not_dir.code, DeviceFileErrorCode::NotAFile);
    }
}
