//! Application and boundary verification of facts recovery plans.

use crate::git::{GitCommitAuthor, GitError, GitLogOptions, GitMemoryRepo, MemoryCommit};

use crate::agreements::{
    AGREEMENTS_PATH, agreement_items_of, read_agreements_at, read_agreements_worktree,
};

use super::mutation_plan::FactsApplyRecovery;
use super::recovery_mutation::{
    FactsMutationError, FactsMutationTransaction, FactsOwnershipLostError,
};
use super::recovery_ownership::{capture_owned_facts_state, same_facts_owned_state};

/// Result of applying a facts recovery plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactsRecoveryResult {
    Committed {
        sha: String,
        affected_paths: Vec<String>,
    },
    ParentDirty {
        detail: Option<String>,
    },
}

impl FactsRecoveryResult {
    /// Return the outcome string: "committed" or "parent_dirty".
    pub fn outcome(&self) -> &str {
        match self {
            Self::Committed { .. } => "committed",
            Self::ParentDirty { .. } => "parent_dirty",
        }
    }

    /// Commit SHA if committed.
    pub fn sha(&self) -> Option<&str> {
        match self {
            Self::Committed { sha, .. } => Some(sha),
            Self::ParentDirty { .. } => None,
        }
    }

    /// List of affected repository paths.
    pub fn affected_paths(&self) -> &[String] {
        match self {
            Self::Committed { affected_paths, .. } => affected_paths,
            Self::ParentDirty { .. } => &[],
        }
    }

    /// Detail message if parent dirty.
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::ParentDirty { detail } => detail.as_deref(),
            Self::Committed { .. } => None,
        }
    }
}

/// Errors returned by recovery execution.
#[derive(Debug)]
pub enum FactsRecoveryError {
    Invalid(String),
    OwnershipLost(FactsOwnershipLostError),
    Git(GitError),
}

impl std::fmt::Display for FactsRecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(message) => write!(f, "{message}"),
            Self::OwnershipLost(err) => write!(f, "{err}"),
            Self::Git(err) => write!(f, "git error: {err}"),
        }
    }
}

impl std::error::Error for FactsRecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Invalid(_) => None,
            Self::OwnershipLost(err) => Some(err),
            Self::Git(err) => Some(err),
        }
    }
}

impl From<FactsOwnershipLostError> for FactsRecoveryError {
    fn from(err: FactsOwnershipLostError) -> Self {
        Self::OwnershipLost(err)
    }
}

impl From<GitError> for FactsRecoveryError {
    fn from(err: GitError) -> Self {
        Self::Git(err)
    }
}

impl From<FactsMutationError> for FactsRecoveryError {
    fn from(err: FactsMutationError) -> Self {
        match err {
            FactsMutationError::OwnershipLost(e) => Self::OwnershipLost(e),
            FactsMutationError::Git(e) => Self::Git(e),
        }
    }
}

/// Locate the commit that records a facts batch receipt, matched by its `Omo-Facts-Batch` trailer.
///
/// Port of `findFactsBatchReceipt` (`facts/recovery.ts:10`): the batch id is the trailer value, so
/// the lookup is a linear scan over the repository log and the first match is the receipt.
pub fn find_facts_batch_receipt(
    repo: &GitMemoryRepo,
    batch_id: &str,
) -> Result<Option<MemoryCommit>, GitError> {
    // Prefilter in git on the bare id: every commit whose parsed trailer equals `batch_id` has it as
    // a substring of the message, so this is a superset and the exact trailer check below decides.
    let options = GitLogOptions {
        grep: (!batch_id.is_empty()).then(|| batch_id.to_string()),
        ..Default::default()
    };
    Ok(repo.log(Some(&options))?.into_iter().find(|commit| {
        commit.trailers.get("Omo-Facts-Batch").map(String::as_str) == Some(batch_id)
    }))
}

/// Apply a pre-computed facts recovery plan to the repository.
pub fn apply_facts_recovery(
    repo: &GitMemoryRepo,
    recovery: &FactsApplyRecovery,
    record_count: usize,
    author: &GitCommitAuthor,
) -> Result<FactsRecoveryResult, FactsRecoveryError> {
    apply_facts_recovery_with_input(repo, recovery, record_count, author, None)
}

pub fn apply_facts_recovery_with_input(
    repo: &GitMemoryRepo,
    recovery: &FactsApplyRecovery,
    record_count: usize,
    author: &GitCommitAuthor,
    mut input: Option<super::agreement_recovery::AgreementRecoveryInput<'_>>,
) -> Result<FactsRecoveryResult, FactsRecoveryError> {
    let Some(observed) = capture_owned_facts_state(repo, recovery)? else {
        return Ok(FactsRecoveryResult::ParentDirty { detail: None });
    };
    let Some(boundary) = capture_owned_facts_state(repo, recovery)? else {
        return Ok(FactsRecoveryResult::ParentDirty { detail: None });
    };
    if !same_facts_owned_state(&observed, &boundary) {
        return Ok(FactsRecoveryResult::ParentDirty { detail: None });
    }

    let effective =
        super::agreement_recovery::upgrade_agreement_recovery(repo, recovery, input.as_ref())?;
    let pre = effective
        .paths
        .iter()
        .map(|entry| (entry.path.clone(), entry.pre.clone()))
        .collect();
    let mut transaction = FactsMutationTransaction::new(repo, recovery, &boundary);
    let run_tx = (|| -> Result<FactsRecoveryResult, FactsRecoveryError> {
        transaction.restore_pre_state()?;
        if effective != *recovery {
            if let Some(publish) = input
                .as_mut()
                .and_then(|input| input.publish_recovery.as_mut())
            {
                publish(&effective)?;
            }
            transaction = FactsMutationTransaction::new(repo, &effective, &pre);
        }
        transaction.apply_post_state()?;
        let paths: Vec<String> = recovery.paths.iter().map(|e| e.path.clone()).collect();
        let evicted = evicted_agreement_ids(repo, recovery)?;
        let message = commit_message(
            &recovery.batch_id,
            record_count,
            &stripped_header_paths(repo, &effective)?,
            &evicted,
        );
        let commit_res = repo
            .commit_prepared(&paths, &message, author)
            .map_err(FactsRecoveryError::Git)?;
        Ok(FactsRecoveryResult::Committed {
            sha: commit_res.sha,
            affected_paths: paths,
        })
    })();

    match run_tx {
        Ok(res) => Ok(res),
        Err(err) => {
            let _ = transaction.rollback();
            match err {
                FactsRecoveryError::OwnershipLost(lost) => {
                    let detail = lost
                        .recovery_path
                        .map(|p| format!("Foreign entry retained at {}", p.display()));
                    Ok(FactsRecoveryResult::ParentDirty { detail })
                }
                other => Err(other),
            }
        }
    }
}

/// The facts lane removes an agreement only by evicting it at the cap; each removal is named in the commit.
fn evicted_agreement_ids(
    repo: &GitMemoryRepo,
    recovery: &FactsApplyRecovery,
) -> Result<Vec<String>, FactsRecoveryError> {
    if !recovery
        .paths
        .iter()
        .any(|entry| entry.path == AGREEMENTS_PATH)
    {
        return Ok(Vec::new());
    }
    let worktree = read_agreements_worktree(&repo.dir).map_err(GitError::Io)?;
    let after: std::collections::HashSet<String> = agreement_items_of(worktree.as_deref())
        .into_iter()
        .map(|item| item.id)
        .collect();
    let before = read_agreements_at(repo, &recovery.head_before_apply)?;
    Ok(agreement_items_of(before.as_deref())
        .into_iter()
        .filter(|item| !after.contains(&item.id))
        .map(|item| item.id)
        .collect())
}

fn commit_message(batch_id: &str, count: usize, stripped: &[String], evicted: &[String]) -> String {
    let fact_word = if count == 1 { "fact" } else { "facts" };
    let mut message = format!(
        "chore(facts): extract {count} {fact_word}\n\nGenerated-By: facts-extractor\nOmo-Writer: facts-extractor\nOmo-Facts-Batch: {batch_id}"
    );
    for path in stripped {
        message.push_str(&format!("\nOmo-Provenance-Stripped: {path}"));
    }
    for id in evicted {
        message.push_str(&format!("\nOmo-Agreement-Evicted: {id}"));
    }
    message
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;

fn stripped_header_paths(
    repo: &GitMemoryRepo,
    recovery: &FactsApplyRecovery,
) -> Result<Vec<String>, FactsRecoveryError> {
    let oids: Vec<_> = recovery
        .paths
        .iter()
        .filter_map(|entry| entry.pre.index.as_ref().map(|index| index.oid.clone()))
        .collect();
    let blobs = repo.read_blobs(&oids)?;
    let mut stripped = Vec::new();
    for entry in &recovery.paths {
        let before = entry
            .pre
            .index
            .as_ref()
            .and_then(|index| blobs.get(&index.oid));
        let written = std::fs::read_to_string(repo.dir.join(&entry.path)).map_err(GitError::Io)?;
        if super::display::stripped_legacy_user_header(before.map(String::as_str), &written) {
            stripped.push(entry.path.clone());
        }
    }
    Ok(stripped)
}

pub struct AgreementRecoveryInput<'a> {
    pub batch: &'a super::extraction::FactsBatch,
    pub policy: &'a crate::agreements::AgreementsPolicy,
    pub publish: &'a mut dyn FnMut(&FactsApplyRecovery) -> Result<(), String>,
}
pub fn apply_facts_recovery_with_agreements(
    repo: &GitMemoryRepo,
    recovery: &FactsApplyRecovery,
    count: usize,
    author: &GitCommitAuthor,
    input: Option<AgreementRecoveryInput<'_>>,
) -> Result<FactsRecoveryResult, FactsRecoveryError> {
    match input {
        None => apply_facts_recovery(repo, recovery, count, author),
        Some(input) => {
            let mut publish = |next: &FactsApplyRecovery| {
                (input.publish)(next).map_err(FactsRecoveryError::Invalid)
            };
            apply_facts_recovery_with_input(
                repo,
                recovery,
                count,
                author,
                Some(super::agreement_recovery::AgreementRecoveryInput {
                    batch: input.batch,
                    agreements_policy: Some(input.policy),
                    publish_recovery: Some(&mut publish),
                }),
            )
        }
    }
}
