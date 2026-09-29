//! Argument formatting and path normalization helpers for Git invocations.

use std::collections::BTreeSet;

use super::errors::GitError;
use super::exec::GitExecResult;
use super::repo_types::GitCommitAuthor;

/// Builds author command flags formatted as `--author=Name <email>`.
pub fn author_flags(author: &GitCommitAuthor) -> Vec<String> {
    let name = if author.author_name.trim().is_empty() {
        author.agent_id.trim()
    } else {
        author.author_name.trim()
    };
    let email = author
        .author_email
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| e.to_string())
        .unwrap_or_else(|| format!("{}@omo.local", author.agent_id.trim()));
    vec![format!("--author={name} <{email}>")]
}

/// Normalizes a slice of paths to deduplicated forward-slash relative pathspecs.
pub fn normalize_pathspecs(paths: &[impl AsRef<str>]) -> Vec<String> {
    let mut unique = BTreeSet::new();
    let mut result = Vec::new();
    for p in paths {
        let norm = p.as_ref().replace('\\', "/");
        let trimmed = norm.trim();
        if !trimmed.is_empty() && unique.insert(trimmed.to_string()) {
            result.push(trimmed.to_string());
        }
    }
    result
}

/// Validates and normalizes seed file relative paths, rejecting traversal segments.
pub fn normalize_seed_path(path: &str) -> Result<String, GitError> {
    let normalized = path.replace('\\', "/");
    let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    if normalized.is_empty()
        || normalized.starts_with('/')
        || segments.iter().any(|s| *s == "." || *s == "..")
    {
        return Err(GitError::InvalidPath(format!(
            "Invalid memory seed path: {path}"
        )));
    }
    Ok(segments.join("/"))
}

/// Converts a failed command result into a typed GitCommandError.
pub fn command_error(argv: &[String], result: &GitExecResult) -> GitError {
    GitError::Command {
        argv: argv.to_vec(),
        code: result.code,
        stdout: result.stdout.clone(),
        stderr: result.stderr.clone(),
    }
}
