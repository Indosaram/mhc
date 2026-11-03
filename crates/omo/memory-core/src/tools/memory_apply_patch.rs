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

use crate::memfs::describe_description_violation;

use super::agreements_write_guard::{refuse_agreements_violation, restore_paths};
use super::claim_frontmatter::{ClaimReceipt, has_legacy_user_header, restamp_content};
use super::memfs::{
    MemoryFrontmatter, ValidateMemoryPathOptions, parse_memory_file, render_memory_file,
    validate_memory_path,
};
use super::memory::{
    MemoryToolCommit, MemoryToolLock, MemoryToolProvenance, MemoryToolProvenanceInput,
    MemoryToolResult, MemoryWriter, memory_commit_message,
};
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
///
/// Validation happens before the lock; everything from the clean check through the commit runs
/// inside the `memory-write` lock domain (pin `memory-apply-patch.ts:58-112`).
pub fn run_memory_apply_patch(
    repo: &GitMemoryRepo,
    params: &MemoryApplyPatchParams,
    lock: Option<&dyn MemoryToolLock>,
) -> Result<MemoryApplyPatchResult, MemoryToolError> {
    let reason = params.reason.trim();
    if reason.is_empty() {
        return Err(err("'reason' must be a non-empty string"));
    }
    if params.input.trim().is_empty() {
        return Err(err("'input' must be a non-empty string"));
    }

    let Some(lock) = lock else {
        return apply_and_commit(repo, params, reason);
    };
    // The lock seam carries the memory tool's result type; the patch result rides out beside it.
    let mut outcome = None;
    lock.with_lock(MEMORY_WRITE_LOCK_DOMAIN, &mut || {
        let result = apply_and_commit(repo, params, reason)?;
        let provenance = MemoryToolProvenance {
            agent_id: params.author.agent_id.clone(),
            author_name: params.author.author_name.clone(),
            timestamp: now_iso(),
            commit_sha: result.commit.as_ref().map(|commit| commit.sha.clone()),
            lock_domain: MEMORY_WRITE_LOCK_DOMAIN.to_string(),
        };
        let carried = MemoryToolResult {
            result: result.message.clone(),
            commit: result.commit.clone(),
            provenance,
        };
        outcome = Some(result);
        Ok(carried)
    })?;
    outcome.ok_or_else(|| err("the memory-write lock did not run the patch"))
}

const MEMORY_WRITE_LOCK_DOMAIN: &str = "memory-write";

fn apply_and_commit(
    repo: &GitMemoryRepo,
    params: &MemoryApplyPatchParams,
    reason: &str,
) -> Result<MemoryApplyPatchResult, MemoryToolError> {
    repo.clean_check().map_err(|e| err(e.to_string()))?;

    // Parse and hunk errors already carry their own `memory apply_patch:` prefix (pin
    // `memory-apply-patch.ts:105-110`): they pass through unchanged.
    let operations =
        parse_memory_patch(&params.input).map_err(|e| MemoryToolError::new(e.message))?;
    let (affected_paths, stripped_paths) = apply_operations(
        &repo.dir,
        &operations,
        params.provenance.as_ref().and_then(|p| p.claim.as_ref()),
    )?;
    if let Some(violation) =
        refuse_agreements_violation(repo, &affected_paths).map_err(|e| err(e.to_string()))?
    {
        return Err(err(violation));
    }

    let commit_res = repo.commit_write(
        &affected_paths,
        &memory_commit_message(
            reason,
            params.provenance.as_ref(),
            MemoryWriter::MemoryTool,
            &stripped_paths,
        ),
        &params.author,
    );

    let commit_info = match commit_res {
        Ok(res) => res,
        Err(GitError::NoEffectiveChanges { .. }) => {
            return Err(err(
                "made no effective changes; nothing was committed. The patched content matched what was already on disk.",
            ));
        }
        Err(err_val) => {
            let refused = err(err_val.to_string());
            // A refused commit (pre-commit hook, git failure) must not leave the applied files
            // behind, or the next memory write fails clean_check on them.
            return Err(match restore_paths(repo, &affected_paths) {
                Ok(()) => refused,
                Err(restore_error) => MemoryToolError::new(format!(
                    "{}\nRolling back the written paths also failed: {restore_error}",
                    refused.message
                )),
            });
        }
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
            subject: commit_subject(reason),
            affected_paths,
        }),
    })
}

fn apply_operations(
    root: &Path,
    operations: &[PatchOperation],
    claim: Option<&ClaimReceipt>,
) -> Result<(Vec<String>, Vec<String>), MemoryToolError> {
    let mut stripped_paths = Vec::new();
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
        read_utf8_text_strict(abs, rel).map(|text| text.replace("\r\n", "\n"))
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
                let label = target_rel.strip_suffix(".md").unwrap_or(&target_rel);
                let normalized = normalize_added_content(label, &raw_content)?;
                let written = restamp_content(None, &normalized, claim).map_err(|e| err(e.0))?;
                pending_writes.insert(target_abs.clone(), written);
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

                let mut next = current.clone();
                for hunk in hunks {
                    next = apply_memory_patch_hunk(&next, hunk, &source_rel)
                        .map_err(|e| MemoryToolError::new(e.message))?;
                }

                let patched = parse_for_tool(&next)?;
                if patched.frontmatter.read_only.as_deref() == Some("true") {
                    return Err(err(format!(
                        "{target_rel} cannot be written with read_only=true"
                    )));
                }
                assert_description_acceptable(&patched.frontmatter.description, &target_rel)?;

                let rendered = render_memory_file(&patched.frontmatter, &patched.body)
                    .map_err(|e| err(e.0.strip_prefix("frontmatter: ").unwrap_or(&e.0)))?;
                let written =
                    restamp_content(Some(&current), &rendered, claim).map_err(|e| err(e.0))?;
                if has_legacy_user_header(Some(&current)) && !has_legacy_user_header(Some(&written))
                {
                    stripped_paths.push(source_rel.clone());
                }
                if source_abs != target_abs {
                    let overwritten = if let Some(pending) = pending_writes.get(&target_abs) {
                        Some(pending.clone())
                    } else if pending_deletes.contains(&target_abs) || !target_abs.exists() {
                        None
                    } else {
                        Some(
                            String::from_utf8_lossy(
                                &fs::read(&target_abs).map_err(|e| err(e.to_string()))?,
                            )
                            .into_owned(),
                        )
                    };
                    if has_legacy_user_header(overwritten.as_deref()) {
                        stripped_paths.push(target_rel.clone());
                    }
                }
                pending_writes.insert(target_abs.clone(), written);
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

    // A delete of a path this patch never saw on disk (a chained move's intermediate) is not a
    // real change: it leaves the affected set so the commit only names paths git knows about.
    for path in pending_deletes {
        if path.exists() {
            fs::remove_file(&path).map_err(|e| err(e.to_string()))?;
        } else {
            affected_paths.remove(&to_relative(root, &path));
        }
    }

    Ok((affected_paths.into_iter().collect(), stripped_paths))
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

fn normalize_added_content(label: &str, raw_content: &str) -> Result<String, MemoryToolError> {
    let rendered = match parse_memory_file(raw_content) {
        Ok(parsed) => {
            assert_description_acceptable(&parsed.frontmatter.description, label)?;
            render_memory_file(&parsed.frontmatter, &parsed.body)
        }
        Err(_) => {
            let frontmatter = MemoryFrontmatter {
                description: format!("Memory block {label}"),
                read_only: None,
                kind: None,
                aliases: None,
                extra: Default::default(),
            };
            render_memory_file(&frontmatter, raw_content)
        }
    };
    rendered.map_err(|e| err(e.0.strip_prefix("frontmatter: ").unwrap_or(&e.0)))
}

fn assert_description_acceptable(description: &str, path: &str) -> Result<(), MemoryToolError> {
    match describe_description_violation(description) {
        Some(violation) => Err(err(format!("{path}: {violation}"))),
        None => Ok(()),
    }
}

/// First line of the reason (pin `memory-apply-patch.ts:101`).
fn commit_subject(reason: &str) -> String {
    reason
        .split('\n')
        .next()
        .unwrap_or_default()
        .trim_end_matches('\r')
        .to_string()
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
