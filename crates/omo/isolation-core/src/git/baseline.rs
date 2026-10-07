use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::backend::{IsolationError, Result};
use crate::git::command::{exists, run_git, str_args, GitOptions, GitOutput};
use crate::util::relative_path;

pub const ISOLATION_BASELINE_MAX_CONTENT_BYTES: u64 = 1024 * 1024 * 1024;
const BASELINE_GIT_TIMEOUT_MS: u64 = 10_000;

pub fn baseline_too_large(
    repo_root: &Path,
    content_bytes: Option<u64>,
    budget_bytes: u64,
) -> IsolationError {
    IsolationError::BaselineTooLarge {
        repo_root: repo_root.to_path_buf(),
        content_bytes,
        budget_bytes,
        message: format!(
            "Working tree at {} exceeds the {budget_bytes}-byte isolation snapshot budget. Commit or gitignore bulk content before isolation.",
            repo_root.display()
        ),
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoBaseline {
    pub repo_root: PathBuf,
    pub head_commit: String,
    pub staged: String,
    pub unstaged: String,
    pub untracked_files: Vec<String>,
    pub untracked_patch: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NestedBaseline {
    pub relative_path: String,
    pub baseline: RepoBaseline,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeBaseline {
    pub root: RepoBaseline,
    pub nested: Vec<NestedBaseline>,
}

#[derive(Debug, Clone)]
pub struct BaselineReadRetryDetails {
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub timeout_ms: u64,
}

pub type BaselineRunGit = Arc<dyn Fn(&[String], &GitOptions) -> Result<GitOutput> + Send + Sync>;
pub type BaselineRetryHandler = Arc<dyn Fn(&BaselineReadRetryDetails) + Send + Sync>;

#[derive(Clone, Default)]
pub struct BaselineCaptureOptions {
    pub on_read_retry: Option<BaselineRetryHandler>,
    pub run_git: Option<BaselineRunGit>,
}

fn baseline_read_config() -> Vec<String> {
    str_args(&[
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.untrackedCache=false",
    ])
}

fn run_baseline_read(
    args: &[String],
    git_options: &GitOptions,
    capture: &BaselineCaptureOptions,
) -> Result<GitOutput> {
    let mut hardened = git_options.clone();
    let mut hardened_args = baseline_read_config();
    hardened_args.extend_from_slice(args);
    hardened
        .env
        .push(("GIT_OPTIONAL_LOCKS".to_string(), "0".to_string()));
    if hardened.timeout_ms.is_none() {
        hardened.timeout_ms = Some(BASELINE_GIT_TIMEOUT_MS);
    }
    let attempt = || match &capture.run_git {
        Some(execute) => execute(&hardened_args, &hardened),
        None => run_git(&hardened_args, &hardened),
    };
    match attempt() {
        Ok(output) => Ok(output),
        Err(error) => {
            let timeout_ms = match &error {
                IsolationError::GitTimeout { timeout_ms, .. } => *timeout_ms,
                _ => return Err(error),
            };
            let details = BaselineReadRetryDetails {
                args: args.to_vec(),
                cwd: git_options.cwd.clone(),
                timeout_ms,
            };
            match &capture.on_read_retry {
                Some(handler) => handler(&details),
                None => eprintln!("[isolation-core] retrying timed-out read-only Git command"),
            }
            attempt()
        }
    }
}

fn capture_stream(
    repo_root: &Path,
    args: &[String],
    allowed_exit_codes: Option<Vec<i32>>,
    remaining: &mut u64,
    budget_bytes: u64,
    capture: &BaselineCaptureOptions,
) -> Result<String> {
    let mut options = GitOptions::new(repo_root.to_path_buf());
    options.allowed_exit_codes = allowed_exit_codes;
    options.max_output_bytes = Some(*remaining);
    let repo_root_owned = repo_root.to_path_buf();
    options.output_limit_error = Some(Arc::new(move || {
        baseline_too_large(&repo_root_owned, None, budget_bytes)
    }));
    let output = run_baseline_read(args, &options, capture)?;
    *remaining = remaining.saturating_sub(output.stdout.len() as u64);
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub fn discover_nested_repos(repo_root: &Path) -> Result<Vec<String>> {
    let status = run_baseline_read(
        &str_args(&["submodule", "status"]),
        &GitOptions::new(repo_root.to_path_buf()),
        &BaselineCaptureOptions::default(),
    )?;
    let status = String::from_utf8_lossy(&status.stdout).into_owned();
    let mut submodules: HashSet<String> = HashSet::new();
    for line in status.split('\n').filter(|line| !line.is_empty()) {
        let mut rest: String = line.chars().skip(42).collect();
        if rest.ends_with(')') && let Some(index) = rest.rfind(" (") {
            rest.truncate(index);
        }
        submodules.insert(rest);
    }
    let mut result = Vec::new();
    walk_nested(repo_root, repo_root, &submodules, &mut result)?;
    result.sort();
    Ok(result)
}

fn walk_nested(
    root: &Path,
    dir: &Path,
    submodules: &HashSet<String>,
    result: &mut Vec<String>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" || name == "node_modules" {
            continue;
        }
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let full = dir.join(&name);
        let relative = relative_path(root, &full).to_string_lossy().replace('\\', "/");
        if submodules.contains(&relative) {
            continue;
        }
        if exists(&full.join(".git"))? {
            result.push(relative);
        } else {
            walk_nested(root, &full, submodules, result)?;
        }
    }
    Ok(())
}

pub fn capture_repo_baseline(
    repo_root: &Path,
    budget_bytes: u64,
    capture: &BaselineCaptureOptions,
) -> Result<RepoBaseline> {
    let mut remaining = budget_bytes;
    let mut head_options = GitOptions::new(repo_root.to_path_buf());
    head_options.allowed_exit_codes = Some(vec![0, 1]);
    let head = run_baseline_read(
        &str_args(&["rev-parse", "--verify", "--quiet", "HEAD"]),
        &head_options,
        capture,
    )?;
    let head_commit = String::from_utf8_lossy(&head.stdout).trim().to_string();
    let diff_args = str_args(&[
        "diff",
        "--binary",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=all",
    ]);
    let mut staged_args = diff_args.clone();
    staged_args.push("--cached".to_string());
    let staged = capture_stream(
        repo_root,
        &staged_args,
        None,
        &mut remaining,
        budget_bytes,
        capture,
    )?;
    let unstaged = capture_stream(
        repo_root,
        &diff_args,
        None,
        &mut remaining,
        budget_bytes,
        capture,
    )?;
    let listed = capture_stream(
        repo_root,
        &str_args(&["ls-files", "--others", "--exclude-standard", "-z"]),
        None,
        &mut remaining,
        budget_bytes,
        capture,
    )?;
    let mut untracked_files: Vec<String> = Vec::new();
    let mut content_bytes = budget_bytes - remaining;
    for file in listed.split('\0').filter(|file| !file.is_empty()) {
        let stat = std::fs::symlink_metadata(repo_root.join(file))?;
        // Git reports an embedded repository as a directory, not its contents.
        if stat.is_dir() {
            continue;
        }
        content_bytes += stat.len();
        if content_bytes > budget_bytes {
            return Err(baseline_too_large(
                repo_root,
                Some(content_bytes),
                budget_bytes,
            ));
        }
        untracked_files.push(file.to_string());
    }
    let null_path = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let mut patches: Vec<String> = Vec::new();
    for file in &untracked_files {
        patches.push(capture_stream(
            repo_root,
            &str_args(&[
                "diff",
                "--no-index",
                "--binary",
                "--no-ext-diff",
                "--no-textconv",
                "--",
                null_path,
                file,
            ]),
            Some(vec![0, 1]),
            &mut remaining,
            budget_bytes,
            capture,
        )?);
    }
    Ok(RepoBaseline {
        repo_root: repo_root.to_path_buf(),
        head_commit,
        staged,
        unstaged,
        untracked_files,
        untracked_patch: patches.concat(),
    })
}

pub fn capture_baseline(repo_root: &Path, budget_bytes: u64) -> Result<WorktreeBaseline> {
    let root = capture_repo_baseline(repo_root, budget_bytes, &BaselineCaptureOptions::default())?;
    let mut nested = Vec::new();
    for relative_path in discover_nested_repos(repo_root)? {
        let baseline = capture_repo_baseline(
            &repo_root.join(&relative_path),
            budget_bytes,
            &BaselineCaptureOptions::default(),
        )?;
        nested.push(NestedBaseline {
            relative_path,
            baseline,
        });
    }
    Ok(WorktreeBaseline { root, nested })
}

pub fn capture_baseline_default(repo_root: &Path) -> Result<WorktreeBaseline> {
    capture_baseline(repo_root, ISOLATION_BASELINE_MAX_CONTENT_BYTES)
}
