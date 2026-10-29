//! Compare-and-set transaction for applying facts mutations to git index and worktree.

use crate::git::path_state::{GitIndexIdentity, GitPathState, GitWorktreeIdentity};
use crate::git::{GitError, GitMemoryRepo};

use super::mutation_plan::FactsApplyRecovery;
use super::recovery_ownership::{FactsOwnedState, same_identity};

/// Error raised when another writer changed a path or worktree state during mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsOwnershipLostError {
    pub message: String,
    pub recovery_path: Option<std::path::PathBuf>,
}

impl FactsOwnershipLostError {
    /// Create a new ownership lost error with message and optional recovery path.
    pub fn new(message: impl Into<String>, recovery_path: Option<std::path::PathBuf>) -> Self {
        Self {
            message: message.into(),
            recovery_path,
        }
    }
}

impl std::fmt::Display for FactsOwnershipLostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for FactsOwnershipLostError {}

enum MutationRecord {
    Index {
        path: String,
        before: Option<GitIndexIdentity>,
        after: Option<GitIndexIdentity>,
    },
    Worktree {
        path: String,
        before: GitWorktreeIdentity,
        after: GitWorktreeIdentity,
    },
}

/// Errors returned during facts mutation transactions.
#[derive(Debug)]
pub enum FactsMutationError {
    OwnershipLost(FactsOwnershipLostError),
    Git(GitError),
}

impl std::fmt::Display for FactsMutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OwnershipLost(err) => write!(f, "{err}"),
            Self::Git(err) => write!(f, "git error: {err}"),
        }
    }
}

impl std::error::Error for FactsMutationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::OwnershipLost(err) => Some(err),
            Self::Git(err) => Some(err),
        }
    }
}

impl From<FactsOwnershipLostError> for FactsMutationError {
    fn from(err: FactsOwnershipLostError) -> Self {
        Self::OwnershipLost(err)
    }
}

impl From<GitError> for FactsMutationError {
    fn from(err: GitError) -> Self {
        Self::Git(err)
    }
}

/// Compare-and-set transaction for applying facts mutations to git index and worktree.
pub struct FactsMutationTransaction<'a> {
    repo: &'a GitMemoryRepo,
    recovery: &'a FactsApplyRecovery,
    initial: &'a FactsOwnedState,
    mutations: Vec<MutationRecord>,
}

impl<'a> FactsMutationTransaction<'a> {
    /// Create a new mutation transaction.
    pub fn new(
        repo: &'a GitMemoryRepo,
        recovery: &'a FactsApplyRecovery,
        initial: &'a FactsOwnedState,
    ) -> Self {
        Self {
            repo,
            recovery,
            initial,
            mutations: Vec::new(),
        }
    }

    /// Restore index and worktree to pre-mutation states before applying target changes.
    pub fn restore_pre_state(&mut self) -> Result<(), FactsMutationError> {
        let recovery = self.recovery;
        for entry in &recovery.paths {
            let initial = self.initial_state(&entry.path)?.clone();
            self.compare_and_set_index(
                &entry.path,
                initial.index.as_ref(),
                entry.pre.index.as_ref(),
            )?;
        }
        for entry in &recovery.paths {
            let initial = self.initial_state(&entry.path)?.clone();
            self.compare_and_set_worktree(&entry.path, &initial.worktree, &entry.pre.worktree)?;
        }
        Ok(())
    }

    /// Apply post-mutation identities to index and worktree.
    pub fn apply_post_state(&mut self) -> Result<(), FactsMutationError> {
        for entry in &self.recovery.paths {
            self.compare_and_set_index(
                &entry.path,
                entry.pre.index.as_ref(),
                Some(&entry.post.index),
            )?;
        }
        for entry in &self.recovery.paths {
            self.compare_and_set_worktree(&entry.path, &entry.pre.worktree, &entry.post.worktree)?;
        }
        Ok(())
    }

    /// Rollback all performed mutations in reverse order.
    pub fn rollback(&mut self) -> Result<(), FactsMutationError> {
        for mutation in self.mutations.split_off(0).into_iter().rev() {
            match mutation {
                MutationRecord::Index {
                    path,
                    before,
                    after,
                } => {
                    let _ = self.repo.path_state.write_index_if_identity(
                        &path,
                        after.as_ref(),
                        before.as_ref(),
                    );
                }
                MutationRecord::Worktree {
                    path,
                    before,
                    after,
                } => {
                    let _ = self
                        .repo
                        .path_state
                        .write_worktree_if_identity(&path, &after, &before);
                }
            }
        }
        Ok(())
    }

    fn initial_state(&self, path: &str) -> Result<&GitPathState, FactsOwnershipLostError> {
        self.initial.get(path).ok_or_else(|| {
            FactsOwnershipLostError::new(format!("Missing initial facts state: {path}"), None)
        })
    }

    fn compare_and_set_index(
        &mut self,
        path: &str,
        expected: Option<&GitIndexIdentity>,
        next: Option<&GitIndexIdentity>,
    ) -> Result<(), FactsMutationError> {
        if same_identity(&expected, &next) {
            return Ok(());
        }
        if !self
            .repo
            .path_state
            .write_index_if_identity(path, expected, next)?
        {
            return Err(FactsOwnershipLostError::new(
                format!("Facts ownership changed: {path}:index"),
                None,
            )
            .into());
        }
        self.mutations.push(MutationRecord::Index {
            path: path.to_string(),
            before: expected.cloned(),
            after: next.cloned(),
        });
        Ok(())
    }

    fn compare_and_set_worktree(
        &mut self,
        path: &str,
        expected: &GitWorktreeIdentity,
        next: &GitWorktreeIdentity,
    ) -> Result<(), FactsMutationError> {
        if same_identity(expected, next) {
            return Ok(());
        }
        let written = self
            .repo
            .path_state
            .write_worktree_if_identity(path, expected, next)?;
        if !written {
            let recovery_path = self.repo.path_state.consume_ownership_loss_recovery_path();
            return Err(FactsOwnershipLostError::new(
                format!("Facts ownership changed: {path}:worktree"),
                recovery_path,
            )
            .into());
        }
        self.mutations.push(MutationRecord::Worktree {
            path: path.to_string(),
            before: expected.clone(),
            after: next.clone(),
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "recovery_mutation_tests.rs"]
mod tests;
