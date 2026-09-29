//! Per-member git worktrees: validation, creation, removal and orphan discovery.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::error::{Result, TeamCoreError};
use crate::path_util;

/// The worktree-manager config (`TeamModeConfig` in manager.ts).
#[derive(Debug, Clone, Default)]
pub struct WorktreeConfig {
    pub worktree_base_dir: Option<String>,
}

/// A git invocation's exit code and stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitOutput {
    pub code: i32,
    pub stderr: String,
}

/// Runs `git <args>` (the TS `gitCommandRunner`, injectable via [`create_worktree_with`]).
pub type GitRunner<'a> = &'a dyn Fn(&[String]) -> GitOutput;

fn run_git_in(args: &[String], cwd: Option<&Path>) -> (GitOutput, String) {
    let mut command = Command::new("git");
    command.args(args).stdin(Stdio::null());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    match command.output() {
        Ok(output) => (
            GitOutput {
                code: output.status.code().unwrap_or(-1),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            },
            String::from_utf8_lossy(&output.stdout).into_owned(),
        ),
        Err(error) => (
            GitOutput {
                code: -1,
                stderr: error.to_string(),
            },
            String::new(),
        ),
    }
}

/// The default runner: spawn the real `git`.
#[must_use]
pub fn run_git(args: &[String]) -> GitOutput {
    run_git_in(args, None).0
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_owned()).collect()
}

/// `isGitAvailable`.
#[must_use]
pub fn is_git_available(runner: GitRunner<'_>) -> bool {
    runner(&strings(&["--version"])).code == 0
}

/// `validateWorktreeSpec`: a './', '../' or '/' path with at most two '..' segments.
pub fn validate_worktree_spec(spec: &str) -> Result<()> {
    let prefixed = ["./", "../", "/"].iter().any(|prefix| {
        spec.strip_prefix(prefix)
            .is_some_and(|rest| !rest.is_empty())
    });
    let parent_segments = spec.split('/').filter(|segment| *segment == "..").count();
    if !prefixed || parent_segments > 2 {
        return Err(TeamCoreError::message(
            "worktreePath must be a filesystem path (relative './...', '../...' or absolute '/...')",
        ));
    }
    Ok(())
}

/// `createWorktree` with the real git.
pub fn create_worktree(
    repo_root: &Path,
    team_run_id: &str,
    member_name: &str,
    worktree_path: &str,
    config: &WorktreeConfig,
) -> Result<PathBuf> {
    create_worktree_with(
        repo_root,
        team_run_id,
        member_name,
        worktree_path,
        config,
        &run_git,
    )
}

/// `createWorktree`: `git -C <repo> worktree add --detach <abs>`.
pub fn create_worktree_with(
    repo_root: &Path,
    _team_run_id: &str,
    _member_name: &str,
    worktree_path: &str,
    _config: &WorktreeConfig,
    runner: GitRunner<'_>,
) -> Result<PathBuf> {
    validate_worktree_spec(worktree_path)?;
    if !is_git_available(runner) {
        return Err(TeamCoreError::GitUnavailable);
    }
    let absolute = if Path::new(worktree_path).is_absolute() {
        PathBuf::from(worktree_path)
    } else {
        path_util::resolve(repo_root, &[worktree_path])
    };
    let result = runner(&[
        "-C".to_owned(),
        repo_root.display().to_string(),
        "worktree".to_owned(),
        "add".to_owned(),
        "--detach".to_owned(),
        absolute.display().to_string(),
    ]);
    if result.code != 0 {
        let stderr = result.stderr.trim();
        return Err(TeamCoreError::message(if stderr.is_empty() {
            "git worktree add failed"
        } else {
            stderr
        }));
    }
    Ok(absolute)
}

fn remove_path_force(path: &Path) -> io::Result<()> {
    let result = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) => Err(error),
    };
    match result {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// `removeWorktree`: delete the directory, then deregister it from git.
pub fn remove_worktree(worktree_path: &Path) -> Result<()> {
    remove_path_force(worktree_path)?;
    let path_text = worktree_path.display().to_string();
    let (root_lookup, root_stdout) = run_git_in(
        &strings(&[
            "-C",
            &path_text,
            "rev-parse",
            "--show-superproject-working-tree",
        ]),
        None,
    );
    let superproject = root_stdout.trim().to_owned();
    let has_root = root_lookup.code == 0 && !superproject.is_empty();
    let result = if has_root {
        run_git(&strings(&[
            "-C",
            &superproject,
            "worktree",
            "remove",
            "--force",
            &path_text,
        ]))
    } else {
        run_git(&strings(&["worktree", "remove", "--force", &path_text]))
    };
    if result.code != 0
        && !result.stderr.contains("not a worktree")
        && !result.stderr.contains("not a working tree")
        && !result.stderr.contains("already removed")
    {
        let stderr = result.stderr.trim();
        return Err(TeamCoreError::message(if stderr.is_empty() {
            "git worktree remove failed"
        } else {
            stderr
        }));
    }
    if has_root {
        let _prune = run_git(&strings(&["-C", &superproject, "worktree", "prune"]));
    }
    Ok(())
}

/// `findOrphanWorktrees`: worktrees whose run is missing or not active/shutdown_requested.
#[must_use]
pub fn find_orphan_worktrees(base_dir: &Path, _config: &WorktreeConfig) -> Vec<PathBuf> {
    let mut orphans = Vec::new();
    let worktrees_dir = base_dir.join("worktrees");
    let Ok(team_runs) = fs::read_dir(&worktrees_dir) else {
        return orphans;
    };
    let mut team_run_ids: Vec<String> = team_runs
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    team_run_ids.sort();
    for team_run_id in team_run_ids {
        let team_run_path = worktrees_dir.join(&team_run_id);
        let mut member_names: Vec<String> = fs::read_dir(&team_run_path)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        member_names.sort();
        let state_path = base_dir
            .join("runtime")
            .join(&team_run_id)
            .join("state.json");
        for member_name in member_names {
            let worktree_path = team_run_path.join(&member_name);
            let status = fs::read_to_string(&state_path)
                .ok()
                .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
                .map(|state| {
                    state
                        .get("status")
                        .and_then(|status| status.as_str())
                        .map(str::to_owned)
                });
            match status {
                Some(Some(status)) if status == "active" || status == "shutdown_requested" => {}
                _ => orphans.push(worktree_path),
            }
        }
    }
    orphans
}
