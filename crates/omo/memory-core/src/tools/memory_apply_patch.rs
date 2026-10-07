//! Multi-file patch application tool operating within git-backed memory repositories.

use std::collections::{BTreeMap, BTreeSet};
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
use super::memory::{MemoryToolCommit, MemoryToolLock, MemoryToolProvenanceInput, memory_commit_message};
use super::patch_parser::{PatchOperation, apply_memory_patch_hunk, parse_memory_patch};
use super::soul::{SOUL_EDIT_RESULT_LINE, touches_soul_path};
use super::tool_errors::MemoryToolError;

/// Input parameters for applying a multi-file Codex patch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryApplyPatchParams {
    pub reason: String,
    pub input: String,
    pub author: GitCommitAuthor,
    /// Trusted provenance injected by the registered `ToolCall` hook (pin `memory-apply-patch.ts:242-249`).
    pub provenance: Option<MemoryToolProvenanceInput>,
}

/// Output result produced by a successful patch application.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryApplyPatchResult {
    pub message: String,
    pub commit: Option<MemoryToolCommit>,
}

fn err(msg: impl Into<String>) -> MemoryToolError {
    MemoryToolError::new(format!("memory_apply_patch: {}", msg.into()))
}

/// Applies a multi-file patch atomically, committing the changed files to the repository.
pub fn run_memory_apply_patch(
    repo: &GitMemoryRepo,
    params: &MemoryApplyPatchParams,
    _lock: Option<&dyn MemoryToolLock>,
) -> Result<MemoryApplyPatchResult, MemoryToolError> {
    if params.reason.trim().is_empty() {
        return Err(err("'reason' must be a non-empty string"));
    }
    if params.input.trim().is_empty() {
        return Err(err("'input' must be a non-empty string"));
    }

    repo.clean_check().map_err(|e| err(e.to_string()))?;

    let operations = parse_memory_patch(&params.input).map_err(|e| err(e.message))?;
    let affected_paths = apply_operations(&repo.dir, &operations)?;

    let commit_res = repo.commit_write(
        &affected_paths,
        &memory_commit_message(&params.reason, params.provenance.as_ref()),
        &params.author,
    );

    let commit_info = match commit_res {
        Ok(res) => res,
        Err(GitError::NoEffectiveChanges { .. }) => {
            return Err(err(
                "made no effective changes; nothing was committed. The patched content matched what was already on disk.",
            ));
        }
        Err(err_val) => return Err(err(err_val.to_string())),
    };

    let has_remote = has_configured_remote(repo);

    let short_sha = if commit_info.sha.len() >= 7 {
        &commit_info.sha[..7]
    } else {
        &commit_info.sha
    };

    let summary = if !has_remote {
        format!("memory_apply_patch committed locally ({short_sha}).")
    } else {
        format!("memory_apply_patch committed ({short_sha}); harness will sync after the turn.")
    };

    let final_message = if touches_soul_path(&affected_paths) {
        format!("{summary}\n{SOUL_EDIT_RESULT_LINE}")
    } else {
        summary
    };

    Ok(MemoryApplyPatchResult {
        message: final_message,
        commit: Some(MemoryToolCommit {
            sha: commit_info.sha,
            committed_at: now_iso(),
        }),
    })
}

fn apply_operations(
    root: &Path,
    operations: &[PatchOperation],
) -> Result<Vec<String>, MemoryToolError> {
    let mut pending_writes: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut pending_deletes: BTreeSet<PathBuf> = BTreeSet::new();
    let mut affected_paths: BTreeSet<String> = BTreeSet::new();

    let resolve_path =
        |input: &str, field_name: &str| -> Result<(PathBuf, String), MemoryToolError> {
            let abs = validate_memory_path(
                root,
                input,
                ValidateMemoryPathOptions {
                    tool_path: true,
                    field_name,
                },
            )
            .map_err(|e| {
                let detail = e.0.strip_prefix("memory path: ").unwrap_or(&e.0);
                let replaced = detail.replace("The memory tool", "The memory_apply_patch tool");
                err(replaced)
            })?;
            let rel = to_relative(root, &abs);
            Ok((abs, rel))
        };

    let load_current = |abs: &Path,
                        rel: &str,
                        pending_w: &BTreeMap<PathBuf, String>,
                        pending_d: &BTreeSet<PathBuf>|
     -> Result<String, MemoryToolError> {
        if pending_d.contains(abs) && !pending_w.contains_key(abs) {
            return Err(err(format!("file not found for update: {rel}")));
        }
        if let Some(content) = pending_w.get(abs) {
            return Ok(content.clone());
        }
        read_utf8_text_strict(abs, rel)
    };

    for op in operations {
        match op {
            PatchOperation::Add {
                target_path,
                content_lines,
            } => {
                let (target_abs, target_rel) = resolve_path(target_path, "Add File path")?;
                if pending_writes.contains_key(&target_abs) {
                    return Err(err(format!(
                        "duplicate add/update target in patch: {target_rel}"
                    )));
                }
                if target_abs.exists() {
                    return Err(err(format!(
                        "cannot add existing memory file: {target_rel}"
                    )));
                }
                let raw_content = content_lines.join("\n");
                let normalized = normalize_added_content(&target_rel, &raw_content);
                pending_writes.insert(target_abs.clone(), normalized);
                pending_deletes.remove(&target_abs);
                affected_paths.insert(target_rel);
            }
            PatchOperation::Delete { target_path } => {
                let (target_abs, target_rel) = resolve_path(target_path, "Delete File path")?;
                let current =
                    load_current(&target_abs, &target_rel, &pending_writes, &pending_deletes)?;
                let parsed = parse_for_tool(&current)?;
                assert_editable(parsed.frontmatter.read_only.as_deref(), &target_rel)?;
                pending_writes.remove(&target_abs);
                pending_deletes.insert(target_abs);
                affected_paths.insert(target_rel);
            }
            PatchOperation::Update {
                source_path,
                target_path,
                hunks,
            } => {
                let (source_abs, source_rel) = resolve_path(source_path, "Update File path")?;
                let (target_abs, target_rel) = resolve_path(target_path, "Move to path")?;
                let current =
                    load_current(&source_abs, &source_rel, &pending_writes, &pending_deletes)?;
                let parsed = parse_for_tool(&current)?;
                assert_editable(parsed.frontmatter.read_only.as_deref(), &source_rel)?;

                let mut next = current;
                for hunk in hunks {
                    next = apply_memory_patch_hunk(&next, hunk, &source_rel)
                        .map_err(|e| MemoryToolError::new(e.message))?;
                }

                if let Ok(next_parsed) = parse_memory_file(&next)
                    && next_parsed.frontmatter.read_only.as_deref() == Some("true")
                {
                    return Err(err(format!(
                        "{target_rel} cannot be written with read_only=true"
                    )));
                }

                pending_writes.insert(target_abs.clone(), next);
                pending_deletes.remove(&target_abs);
                affected_paths.insert(target_rel.clone());

                if source_abs != target_abs {
                    pending_writes.remove(&source_abs);
                    pending_deletes.insert(source_abs);
                    affected_paths.insert(source_rel);
                }
            }
        }
    }

    for (path, content) in pending_writes {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| err(e.to_string()))?;
        }
        fs::write(&path, content).map_err(|e| err(e.to_string()))?;
    }

    for path in pending_deletes {
        if path.exists() {
            fs::remove_file(&path).map_err(|e| err(e.to_string()))?;
        }
    }

    Ok(affected_paths.into_iter().collect())
}

fn to_relative(root: &Path, target: &Path) -> String {
    let rel = target.strip_prefix(root).unwrap_or(target);
    to_git_path(rel)
}

fn parse_for_tool(content: &str) -> Result<super::memfs::ParsedMemoryFile, MemoryToolError> {
    parse_memory_file(content).map_err(|e| {
        let msg = e.0.strip_prefix("frontmatter: ").unwrap_or(&e.0);
        err(msg)
    })
}

fn normalize_added_content(rel_path: &str, raw_content: &str) -> String {
    if let Ok(parsed) = parse_memory_file(raw_content) {
        render_memory_file(&parsed.frontmatter, &parsed.body)
            .unwrap_or_else(|_| raw_content.to_string())
    } else {
        let label = rel_path.strip_suffix(".md").unwrap_or(rel_path);
        let frontmatter = MemoryFrontmatter {
            description: format!("Memory block {label}"),
            read_only: None,
            kind: None,
            aliases: None,
        };
        render_memory_file(&frontmatter, raw_content).unwrap_or_else(|_| raw_content.to_string())
    }
}

/// Pin `hasConfiguredRemote`: any `[remote "<name>"]` section in the repository's git config.
fn has_configured_remote(repo: &GitMemoryRepo) -> bool {
    let config = fs::read_to_string(repo.dir.join(".git").join("config")).unwrap_or_default();
    config.lines().any(is_remote_section_header)
}

fn is_remote_section_header(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("[remote") else {
        return false;
    };
    let Some(first) = rest.chars().next() else {
        return false;
    };
    if !first.is_whitespace() {
        return false;
    }
    let Some(after_quote) = rest.trim_start().strip_prefix('"') else {
        return false;
    };
    let Some((name, tail)) = after_quote.split_once('"') else {
        return false;
    };
    !name.is_empty() && tail.starts_with(']')
}

fn assert_editable(read_only: Option<&str>, path: &str) -> Result<(), MemoryToolError> {
    if read_only == Some("true") {
        return Err(err(format!("{path} is read_only and cannot be modified")));
    }
    Ok(())
}

fn read_utf8_text_strict(path: &Path, rel: &str) -> Result<String, MemoryToolError> {
    let bytes = fs::read(path).map_err(|e| err(format!("failed to read {rel}: {e}")))?;
    let bom = if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xfe {
        Some("UTF-16LE")
    } else if bytes.len() >= 2 && bytes[0] == 0xfe && bytes[1] == 0xff {
        Some("UTF-16BE")
    } else {
        None
    };

    if let Some(kind) = bom {
        return Err(err(format!(
            "failed to read {rel}: File is not valid UTF-8 text: {}. Detected {kind} BOM; convert the file to UTF-8 and retry.",
            path.display()
        )));
    }

    String::from_utf8(bytes).map_err(|_| {
        err(format!(
            "failed to read {rel}: File is not valid UTF-8 text: {}. The file contains bytes that cannot be decoded as UTF-8.",
            path.display()
        ))
    })
}
