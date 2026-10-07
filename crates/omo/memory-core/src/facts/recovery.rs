//! Application and boundary verification of facts recovery plans.

use crate::git::{GitCommitAuthor, GitError, GitMemoryRepo, MemoryCommit};

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
    OwnershipLost(FactsOwnershipLostError),
    Git(GitError),
}

impl std::fmt::Display for FactsRecoveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OwnershipLost(err) => write!(f, "{err}"),
            Self::Git(err) => write!(f, "git error: {err}"),
        }
    }
}

impl std::error::Error for FactsRecoveryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
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
    Ok(repo.log(None)?.into_iter().find(|commit| {
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
    let Some(observed) = capture_owned_facts_state(repo, recovery)? else {
        return Ok(FactsRecoveryResult::ParentDirty { detail: None });
    };
    let Some(boundary) = capture_owned_facts_state(repo, recovery)? else {
        return Ok(FactsRecoveryResult::ParentDirty { detail: None });
    };
    if !same_facts_owned_state(&observed, &boundary) {
        return Ok(FactsRecoveryResult::ParentDirty { detail: None });
    }

    let mut transaction = FactsMutationTransaction::new(repo, recovery, &boundary);
    let run_tx = (|| -> Result<FactsRecoveryResult, FactsRecoveryError> {
        transaction.restore_pre_state()?;
        transaction.apply_post_state()?;
        let paths: Vec<String> = recovery.paths.iter().map(|e| e.path.clone()).collect();
        let message = commit_message(&recovery.batch_id, record_count);
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

fn commit_message(batch_id: &str, count: usize) -> String {
    let fact_word = if count == 1 { "fact" } else { "facts" };
    format!(
        "chore(facts): extract {count} {fact_word}\n\nGenerated-By: facts-extractor\nOmo-Writer: facts-extractor\nOmo-Facts-Batch: {batch_id}"
    )
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
