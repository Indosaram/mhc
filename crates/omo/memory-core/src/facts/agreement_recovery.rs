//! Ownership-checked upgrade of pre-event-version agreement recovery envelopes.
use super::extraction::FactsBatch;
use super::mutation_plan::{FactsApplyRecovery, facts_records_hash};
use super::recovery::FactsRecoveryError;
use crate::agreements::{AGREEMENTS_PATH, AgreementsPolicy};
use crate::git::{GitMemoryRepo, path_state::GitWorktreeIdentity};

pub type RecoveryPublisher<'a> =
    dyn FnMut(&FactsApplyRecovery) -> Result<(), FactsRecoveryError> + 'a;
pub struct AgreementRecoveryInput<'a> {
    pub batch: &'a FactsBatch,
    pub agreements_policy: Option<&'a AgreementsPolicy>,
    pub publish_recovery: Option<&'a mut RecoveryPublisher<'a>>,
}

pub fn upgrade_agreement_recovery(
    repo: &GitMemoryRepo,
    recovery: &FactsApplyRecovery,
    input: Option<&AgreementRecoveryInput<'_>>,
) -> Result<FactsApplyRecovery, FactsRecoveryError> {
    if recovery.agreement_event_version == Some(1) {
        return Ok(recovery.clone());
    }
    let Some(at) = recovery
        .paths
        .iter()
        .position(|entry| entry.path == AGREEMENTS_PATH)
    else {
        return Ok(recovery.clone());
    };
    let invalid = || {
        FactsRecoveryError::Invalid("Legacy agreement recovery requires its original records and an atomic recovery publisher".into())
    };
    let input = input.ok_or_else(invalid)?;
    if input.publish_recovery.is_none()
        || input.batch.batch_id != recovery.batch_id
        || input.batch.agreements.is_none()
        || facts_records_hash(
            &input.batch.records,
            input.batch.agreements.as_deref().unwrap_or_default(),
        ) != recovery.records_hash
    {
        return Err(invalid());
    }
    let ledger = &recovery.paths[at];
    let existing = match &ledger.pre.worktree {
        GitWorktreeIdentity::Missing => None,
        GitWorktreeIdentity::File(file) => {
            let blobs = repo.read_blobs(std::slice::from_ref(&file.oid))?;
            Some(blobs.get(&file.oid).cloned().ok_or_else(|| {
                FactsRecoveryError::Invalid("Missing legacy agreement pre-image".into())
            })?)
        }
    };
    let policy = input.agreements_policy.copied().unwrap_or_default();
    let rendered = super::agreements_target::render_agreements_content(
        existing.as_deref(),
        input.batch.agreements.as_deref().unwrap_or_default(),
        &policy,
    )
    .map_err(|error| FactsRecoveryError::Invalid(error.to_string()))?;
    let content = rendered.content.or(existing).ok_or_else(|| {
        FactsRecoveryError::Invalid("Legacy agreement recovery has no ledger content".into())
    })?;
    let mut upgraded = recovery.clone();
    upgraded.agreement_event_version = Some(1);
    let post = &mut upgraded.paths[at].post;
    post.index.oid = repo
        .path_state
        .hash_index_blob(AGREEMENTS_PATH, &content, true)?;
    if let GitWorktreeIdentity::File(file) = &mut post.worktree {
        file.oid = repo.path_state.hash_worktree_blob(&content, true)?;
    }
    Ok(upgraded)
}

#[cfg(test)]
#[path = "agreement_recovery_tests.rs"]
mod tests;
