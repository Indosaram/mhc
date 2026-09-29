//! Memory tool implementation executing CRUD operations against markdown memory repositories.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::git::GitMemoryRepo;
use crate::git::errors::GitError;
use crate::git::repo_types::GitCommitAuthor;
use crate::support::paths::to_git_path;
use crate::support::time::now_iso;

use super::memfs::{
    MemoryFrontmatter, ValidateMemoryPathOptions, parse_memory_file, render_memory_file,
    validate_memory_path,
};
use super::soul::{SOUL_EDIT_RESULT_LINE, touches_soul_path};
use super::tool_errors::MemoryToolError;

/// Supported CRUD commands for the memory tool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryToolCommand {
    Create,
    StrReplace,
    Insert,
    Delete,
    Rename,
    UpdateDescription,
}

impl MemoryToolCommand {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::StrReplace => "str_replace",
            Self::Insert => "insert",
            Self::Delete => "delete",
            Self::Rename => "rename",
            Self::UpdateDescription => "update_description",
        }
    }
}

/// Parameters for calling the memory tool.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MemoryToolParams {
    pub command: Option<String>,
    pub reason: String,
    pub file_path: Option<String>,
    pub file_text: Option<String>,
    pub description: Option<String>,
    pub old_string: Option<String>,
    pub new_string: Option<String>,
    pub insert_line: Option<i64>,
    pub insert_text: Option<String>,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
}

/// Commit metadata returned by memory tool operations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryToolCommit {
    pub sha: String,
    pub committed_at: String,
}

/// Provenance metadata tracking author attribution and transaction details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryToolProvenance {
    pub agent_id: String,
    pub author_name: String,
    pub timestamp: String,
    pub commit_sha: Option<String>,
    pub lock_domain: String,
}

/// Output result produced by a successful memory tool operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryToolResult {
    pub result: String,
    pub commit: Option<MemoryToolCommit>,
    pub provenance: MemoryToolProvenance,
}

/// Lock abstraction for serializing memory tool operations within a lock domain.
pub trait MemoryToolLock {
    fn with_lock(
        &self,
        domain: &str,
        operation: &mut dyn FnMut() -> Result<MemoryToolResult, MemoryToolError>,
    ) -> Result<MemoryToolResult, MemoryToolError>;
}

struct NoopMemoryToolLock;

impl MemoryToolLock for NoopMemoryToolLock {
    fn with_lock(
        &self,
        _domain: &str,
        operation: &mut dyn FnMut() -> Result<MemoryToolResult, MemoryToolError>,
    ) -> Result<MemoryToolResult, MemoryToolError> {
        operation()
    }
}

fn err(msg: impl Into<String>) -> MemoryToolError {
    MemoryToolError::new(format!("memory: {}", msg.into()))
}

fn require_str<'a>(
    command: &str,
    field_name: &str,
    val: Option<&'a str>,
) -> Result<&'a str, MemoryToolError> {
    match val {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ => Err(err(format!(
            "{command}: '{field_name}' must be a non-empty string"
        ))),
    }
}

/// Executes a memory tool operation against the repository with git commit persistence.
pub fn run_memory_tool(
    repo: &GitMemoryRepo,
    author: &GitCommitAuthor,
    params: &MemoryToolParams,
    lock: Option<&dyn MemoryToolLock>,
) -> Result<MemoryToolResult, MemoryToolError> {
    let default_lock = NoopMemoryToolLock;
    let effective_lock = lock.unwrap_or(&default_lock);

    effective_lock.with_lock("memory:all", &mut || {
        run_memory_tool_inner(repo, author, params)
    })
}

fn run_memory_tool_inner(
    repo: &GitMemoryRepo,
    author: &GitCommitAuthor,
    params: &MemoryToolParams,
) -> Result<MemoryToolResult, MemoryToolError> {
    if params.reason.trim().is_empty() {
        return Err(err("'reason' must be a non-empty string"));
    }

    let command_str = params
        .command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err("'command' must be a non-empty string"))?;

    let root = &repo.dir;
    let mut affected_relative_paths: Vec<String> = Vec::new();

    let mut execute_command = || -> Result<(), MemoryToolError> {
        match command_str {
            "create" => {
                let path_str = require_str("create", "file_path", params.file_path.as_deref())?;
                let desc_str = require_str("create", "description", params.description.as_deref())?;
                let target = validate_path(root, path_str, "file_path")?;
                let rel = to_relative(root, &target);
                affected_relative_paths.push(rel.clone());

                if target.exists() {
                    return Err(err(format!("create: block already exists at {rel}")));
                }

                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|e| err(e.to_string()))?;
                }

                let frontmatter = MemoryFrontmatter {
                    description: desc_str.to_string(),
                    read_only: None,
                    kind: None,
                    aliases: None,
                };
                let body = params.file_text.as_deref().unwrap_or("");
                let rendered = render_memory_file(&frontmatter, body).map_err(|e| err(e.0))?;

                fs::write(&target, rendered).map_err(|e| err(e.to_string()))?;
                Ok(())
            }
            "str_replace" => {
                let path_str =
                    require_str("str_replace", "file_path", params.file_path.as_deref())?;
                let old_str =
                    require_str("str_replace", "old_string", params.old_string.as_deref())?;
                let new_str = params
                    .new_string
                    .as_deref()
                    .ok_or_else(|| err("str_replace: 'new_string' must be a non-empty string"))?;

                let target = validate_path(root, path_str, "file_path")?;
                let rel = to_relative(root, &target);
                affected_relative_paths.push(rel.clone());

                let content = read_existing_file(&target, &rel)?;
                let parsed = parse_memory_file(&content).map_err(|e| err(e.0))?;
                assert_not_readonly(&parsed.frontmatter, &rel)?;

                if !parsed.body.contains(old_str) {
                    return Err(err(
                        "str_replace: old_string was not found in the target memory block",
                    ));
                }
                if old_str == new_str {
                    return Err(err("str_replace made no changes"));
                }

                let updated_body = parsed.body.replacen(old_str, new_str, 1);
                let rendered =
                    render_memory_file(&parsed.frontmatter, &updated_body).map_err(|e| err(e.0))?;

                fs::write(&target, rendered).map_err(|e| err(e.to_string()))?;
                Ok(())
            }
            "insert" => {
                let path_str = require_str("insert", "file_path", params.file_path.as_deref())?;
                let insert_line = params
                    .insert_line
                    .ok_or_else(|| err("insert: 'insert_line' must be a number"))?;
                let insert_text =
                    require_str("insert", "insert_text", params.insert_text.as_deref())?;

                let target = validate_path(root, path_str, "file_path")?;
                let rel = to_relative(root, &target);
                affected_relative_paths.push(rel.clone());

                let content = read_existing_file(&target, &rel)?;
                let parsed = parse_memory_file(&content).map_err(|e| err(e.0))?;
                assert_not_readonly(&parsed.frontmatter, &rel)?;

                let mut lines: Vec<&str> = parsed.body.split('\n').collect();
                let line_idx = if insert_line <= 1 {
                    0
                } else {
                    ((insert_line - 1) as usize).min(lines.len())
                };

                let insert_lines: Vec<&str> = insert_text.split('\n').collect();
                for (offset, line) in insert_lines.into_iter().enumerate() {
                    lines.insert(line_idx + offset, line);
                }

                let updated_body = lines.join("\n");
                if updated_body == parsed.body {
                    return Err(err("insert made no changes"));
                }

                let rendered =
                    render_memory_file(&parsed.frontmatter, &updated_body).map_err(|e| err(e.0))?;

                fs::write(&target, rendered).map_err(|e| err(e.to_string()))?;
                Ok(())
            }
            "delete" => {
                let path_str = require_str("delete", "file_path", params.file_path.as_deref())?;
                let target = validate_path(root, path_str, "file_path")?;
                let rel = to_relative(root, &target);
                affected_relative_paths.push(rel.clone());

                if target.is_dir() {
                    assert_dir_not_readonly(&target, root)?;
                    fs::remove_dir_all(&target).map_err(|e| err(e.to_string()))?;
                } else if target.exists() {
                    let content = read_existing_file(&target, &rel)?;
                    let parsed = parse_memory_file(&content).map_err(|e| err(e.0))?;
                    assert_not_readonly(&parsed.frontmatter, &rel)?;
                    fs::remove_file(&target).map_err(|e| err(e.to_string()))?;
                }
                Ok(())
            }
            "rename" => {
                let old_p = require_str("rename", "old_path", params.old_path.as_deref())?;
                let new_p = require_str("rename", "new_path", params.new_path.as_deref())?;

                let src = validate_path(root, old_p, "old_path")?;
                let dst = validate_path(root, new_p, "new_path")?;

                let src_rel = to_relative(root, &src);
                let dst_rel = to_relative(root, &dst);
                affected_relative_paths.push(src_rel.clone());
                affected_relative_paths.push(dst_rel.clone());

                if !src.exists() {
                    return Err(err(format!("rename: source does not exist at {src_rel}")));
                }
                if dst.exists() {
                    return Err(err(format!(
                        "rename: destination already exists at {dst_rel}"
                    )));
                }

                if src.is_dir() {
                    assert_dir_not_readonly(&src, root)?;
                } else {
                    let content = read_existing_file(&src, &src_rel)?;
                    let parsed = parse_memory_file(&content).map_err(|e| err(e.0))?;
                    assert_not_readonly(&parsed.frontmatter, &src_rel)?;
                }

                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent).map_err(|e| err(e.to_string()))?;
                }

                fs::rename(&src, &dst).map_err(|e| err(e.to_string()))?;
                Ok(())
            }
            "update_description" => {
                let path_str = require_str(
                    "update_description",
                    "file_path",
                    params.file_path.as_deref(),
                )?;
                let desc_str = require_str(
                    "update_description",
                    "description",
                    params.description.as_deref(),
                )?;

                let target = validate_path(root, path_str, "file_path")?;
                let rel = to_relative(root, &target);
                affected_relative_paths.push(rel.clone());

                let content = read_existing_file(&target, &rel)?;
                let mut parsed = parse_memory_file(&content).map_err(|e| err(e.0))?;
                assert_not_readonly(&parsed.frontmatter, &rel)?;

                if parsed.frontmatter.description.trim() == desc_str.trim() {
                    return Err(err("update_description made no changes"));
                }

                parsed.frontmatter.description = desc_str.to_string();
                let rendered =
                    render_memory_file(&parsed.frontmatter, &parsed.body).map_err(|e| err(e.0))?;

                fs::write(&target, rendered).map_err(|e| err(e.to_string()))?;
                Ok(())
            }
            unknown => Err(err(format!("unknown command: {unknown}"))),
        }
    };

    execute_command()?;

    let commit_res = repo.commit_write(&affected_relative_paths, &params.reason, author);

    let commit_info = match commit_res {
        Ok(res) => res,
        Err(GitError::NoEffectiveChanges { .. }) => {
            return Err(err(format!("{command_str} made no effective changes")));
        }
        Err(err_val) => return Err(err(err_val.to_string())),
    };

    let has_remote = repo
        .config_get("remote.origin.url")
        .map_err(|e| err(e.to_string()))?
        .is_some();

    let short_sha = if commit_info.sha.len() >= 7 {
        &commit_info.sha[..7]
    } else {
        &commit_info.sha
    };

    let summary = if !has_remote {
        format!("Memory {command_str} committed locally ({short_sha}).")
    } else {
        format!("Memory {command_str} committed ({short_sha}); harness will sync after the turn.")
    };

    let final_result = if touches_soul_path(&affected_relative_paths) {
        format!("{summary}\n{SOUL_EDIT_RESULT_LINE}")
    } else {
        summary
    };

    let timestamp = now_iso();
    Ok(MemoryToolResult {
        result: final_result,
        commit: Some(MemoryToolCommit {
            sha: commit_info.sha.clone(),
            committed_at: timestamp.clone(),
        }),
        provenance: MemoryToolProvenance {
            agent_id: author.agent_id.clone(),
            author_name: author.author_name.clone(),
            timestamp,
            commit_sha: Some(commit_info.sha),
            lock_domain: "memory:all".to_string(),
        },
    })
}

fn validate_path(root: &Path, path: &str, field_name: &str) -> Result<PathBuf, MemoryToolError> {
    validate_memory_path(
        root,
        path,
        ValidateMemoryPathOptions {
            tool_path: true,
            field_name,
        },
    )
    .map_err(|e| err(e.0.strip_prefix("memory path: ").unwrap_or(&e.0)))
}

fn to_relative(root: &Path, target: &Path) -> String {
    let rel = target.strip_prefix(root).unwrap_or(target);
    to_git_path(rel)
}

fn read_existing_file(path: &Path, display_path: &str) -> Result<String, MemoryToolError> {
    let bytes = fs::read(path).map_err(|e| err(e.to_string()))?;
    if bytes.len() >= 2
        && ((bytes[0] == 0xff && bytes[1] == 0xfe) || (bytes[0] == 0xfe && bytes[1] == 0xff))
    {
        return Err(err(format!(
            "{display_path} is UTF-16 encoded; convert it to UTF-8 before using the memory tool"
        )));
    }
    String::from_utf8(bytes).map_err(|_| {
        err(format!(
            "{display_path} is not valid UTF-8; convert it to UTF-8 before using the memory tool"
        ))
    })
}

fn assert_not_readonly(
    frontmatter: &MemoryFrontmatter,
    display_path: &str,
) -> Result<(), MemoryToolError> {
    if frontmatter.read_only.is_some() {
        return Err(err(format!(
            "{display_path} is read_only and cannot be modified"
        )));
    }
    Ok(())
}

fn assert_dir_not_readonly(dir: &Path, root: &Path) -> Result<(), MemoryToolError> {
    for entry in fs::read_dir(dir).map_err(|e| err(e.to_string()))? {
        let entry = entry.map_err(|e| err(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            assert_dir_not_readonly(&path, root)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            let rel = to_relative(root, &path);
            let content = read_existing_file(&path, &rel)?;
            if let Ok(parsed) = parse_memory_file(&content) {
                assert_not_readonly(&parsed.frontmatter, &rel)?;
            }
        }
    }
    Ok(())
}
