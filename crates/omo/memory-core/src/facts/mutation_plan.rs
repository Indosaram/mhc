//! Planning stage for facts mutations and recovery envelopes.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::git::path_state::{
    GitIndexIdentity, GitPathState, GitWorktreeFileIdentity, GitWorktreeIdentity,
};
use crate::git::{GitError, GitMemoryRepo};
use crate::memfs::{FrontmatterError, MemoryFrontmatter, parse_memory_file, render_memory_file};

use super::extraction::{FactsBatch, FactsExtractionRecord};
use super::person_routing::{
    AliasTieCallback, FactsPeopleRouting, facts_routing_paths, plan_facts_routing,
    render_person_targets,
};

/// Post-mutation target identity for index and worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPostIdentity {
    pub index: GitIndexIdentity,
    pub worktree: GitWorktreeIdentity,
}

/// Recovery path describing state before and after mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsRecoveryPath {
    pub path: String,
    pub pre: GitPathState,
    pub post: FactsPostIdentity,
}

/// Error raised when the parent worktree was dirty during mutation planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsPlanParentDirtyError {
    pub message: String,
}

impl FactsPlanParentDirtyError {
    /// Create a new parent dirty error with the given message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for FactsPlanParentDirtyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for FactsPlanParentDirtyError {}

/// Envelope containing pre-computed hashes and paths for atomic application and recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsApplyRecovery {
    pub version: u32,
    #[serde(rename = "batchId")]
    pub batch_id: String,
    #[serde(rename = "headBeforeApply")]
    pub head_before_apply: String,
    #[serde(rename = "recordsHash")]
    pub records_hash: String,
    pub people: Option<FactsPeopleRouting>,
    pub paths: Vec<FactsRecoveryPath>,
}

/// Errors returned by mutation planning.
#[derive(Debug)]
pub enum MutationPlanError {
    NoHead,
    ParentDirty(FactsPlanParentDirtyError),
    IncompletePlan(String),
    Frontmatter(FrontmatterError),
    Git(GitError),
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for MutationPlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoHead => write!(f, "facts repository has no HEAD"),
            Self::ParentDirty(err) => write!(f, "{err}"),
            Self::IncompletePlan(p) => write!(f, "Incomplete facts mutation plan: {p}"),
            Self::Frontmatter(err) => write!(f, "frontmatter error: {err}"),
            Self::Git(err) => write!(f, "git error: {err}"),
            Self::Io(err) => write!(f, "io error: {err}"),
            Self::Json(err) => write!(f, "json error: {err}"),
        }
    }
}

impl std::error::Error for MutationPlanError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ParentDirty(err) => Some(err),
            Self::Git(err) => Some(err),
            Self::Io(err) => Some(err),
            Self::Json(err) => Some(err),
            Self::Frontmatter(err) => Some(err),
            Self::NoHead | Self::IncompletePlan(_) => None,
        }
    }
}

impl From<FactsPlanParentDirtyError> for MutationPlanError {
    fn from(err: FactsPlanParentDirtyError) -> Self {
        Self::ParentDirty(err)
    }
}

impl From<FrontmatterError> for MutationPlanError {
    fn from(err: FrontmatterError) -> Self {
        Self::Frontmatter(err)
    }
}

impl From<GitError> for MutationPlanError {
    fn from(err: GitError) -> Self {
        Self::Git(err)
    }
}

impl From<std::io::Error> for MutationPlanError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<serde_json::Error> for MutationPlanError {
    fn from(err: serde_json::Error) -> Self {
        Self::Json(err)
    }
}

/// Compute SHA-256 hash of JSON-serialized extraction records.
pub fn facts_records_hash(records: &[FactsExtractionRecord]) -> String {
    let json = serde_json::to_string(records).unwrap_or_default();
    crate::support::sha256::sha256_hex(json.as_bytes())
}

/// Render a facts markdown note with frontmatter and bullet points.
pub fn render_facts_note(
    root: &Path,
    relative_path: &str,
    records: &[FactsExtractionRecord],
) -> Result<String, MutationPlanError> {
    let file_path = root.join(relative_path);
    let (frontmatter, existing_body) = match std::fs::read_to_string(&file_path) {
        Ok(existing) => {
            let parsed = parse_memory_file(&existing)?;
            (parsed.frontmatter, parsed.body)
        }
        Err(_) => {
            let slice_start = relative_path.len().saturating_sub(10);
            let slice_end = relative_path.len().saturating_sub(3);
            let date_slice = relative_path
                .get(slice_start..slice_end)
                .unwrap_or_default();
            (
                MemoryFrontmatter {
                    description: format!("Explicit facts for {date_slice}"),
                    read_only: None,
                    kind: None,
                    aliases: None,
                },
                String::new(),
            )
        }
    };
    let prefix = if existing_body.is_empty() || existing_body.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    let mut bullets = String::new();
    for (index, record) in records.iter().enumerate() {
        if index > 0 {
            bullets.push('\n');
        }
        bullets.push_str(&format!("- [{}] {}", record.date(), record.text()));
    }
    let body = format!("{existing_body}{prefix}{bullets}\n");
    Ok(render_memory_file(&frontmatter, &body)?)
}

/// Plan repository changes and produce an apply recovery envelope.
pub fn plan_facts_mutation(
    repo: &GitMemoryRepo,
    batch: &FactsBatch,
    people: Option<&FactsPeopleRouting>,
    on_alias_tie: Option<AliasTieCallback<'_>>,
) -> Result<FactsApplyRecovery, MutationPlanError> {
    let head_before_apply = repo
        .head()
        .map_err(MutationPlanError::Git)?
        .ok_or(MutationPlanError::NoHead)?;

    let routing = plan_facts_routing(&repo.dir, &batch.records, people, on_alias_tie);
    let paths = facts_routing_paths(&routing);
    let pre = repo
        .path_state
        .capture_all(&paths)
        .map_err(MutationPlanError::Git)?;

    if !repo
        .status(&[] as &[&str])
        .map_err(MutationPlanError::Git)?
        .trim()
        .is_empty()
    {
        return Err(MutationPlanError::ParentDirty(
            FactsPlanParentDirtyError::new("facts repository changed during planning"),
        ));
    }

    let mut content = std::collections::HashMap::new();
    for (path, records) in &routing.notes {
        content.insert(path.clone(), render_facts_note(&repo.dir, path, records)?);
    }

    if let Some(people_cfg) = people.filter(|policy| policy.enabled) {
        content.extend(render_person_targets(
            &repo.dir,
            &routing,
            people_cfg.limits(),
        )?);
    }

    let mut planned = Vec::new();
    for path in &paths {
        let before = pre
            .get(path)
            .ok_or_else(|| MutationPlanError::IncompletePlan(path.clone()))?;
        let value = content
            .get(path)
            .ok_or_else(|| MutationPlanError::IncompletePlan(path.clone()))?;

        let worktree_oid = repo
            .path_state
            .hash_worktree_blob(value, true)
            .map_err(MutationPlanError::Git)?;
        let index_oid = repo
            .path_state
            .hash_index_blob(path, value, true)
            .map_err(MutationPlanError::Git)?;

        let mode = match &before.worktree {
            GitWorktreeIdentity::File(file) => file.mode,
            GitWorktreeIdentity::Missing => 0o644,
        };

        planned.push(FactsRecoveryPath {
            path: path.clone(),
            pre: before.clone(),
            post: FactsPostIdentity {
                index: GitIndexIdentity {
                    mode: before
                        .index
                        .as_ref()
                        .map(|item| item.mode.clone())
                        .unwrap_or_else(|| "100644".to_string()),
                    oid: index_oid,
                },
                worktree: GitWorktreeIdentity::File(GitWorktreeFileIdentity {
                    mode,
                    oid: worktree_oid,
                }),
            },
        });
    }

    Ok(FactsApplyRecovery {
        version: 1,
        batch_id: batch.batch_id.clone(),
        head_before_apply,
        records_hash: facts_records_hash(&batch.records),
        people: people.cloned(),
        paths: planned,
    })
}

#[cfg(test)]
#[path = "mutation_plan_tests.rs"]
mod tests;
