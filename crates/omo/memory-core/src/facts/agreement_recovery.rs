//! Upgrade a legacy agreement post-image only with its original batch and durable publisher.
use super::{
    agreements_target::render_agreements_content,
    extraction::FactsBatch,
    mutation_plan::{FactsApplyRecovery, facts_records_hash},
};
use crate::{
    agreements::{AGREEMENTS_PATH, AgreementsPolicy},
    git::{GitMemoryRepo, path_state::GitWorktreeIdentity},
};
pub fn upgrade_agreement_recovery(
    repo: &GitMemoryRepo,
    recovery: &FactsApplyRecovery,
    batch: &FactsBatch,
    policy: &AgreementsPolicy,
) -> Result<FactsApplyRecovery, String> {
    if recovery.agreement_event_version == Some(1) {
        return Ok(recovery.clone());
    }
    let Some(ledger) = recovery
        .paths
        .iter()
        .find(|entry| entry.path == AGREEMENTS_PATH)
    else {
        return Ok(recovery.clone());
    };
    if batch.batch_id != recovery.batch_id
        || facts_records_hash(
            &batch.records,
            batch.agreements.as_deref().unwrap_or_default(),
        ) != recovery.records_hash
        || batch.agreements.is_none()
    {
        return Err("Legacy agreement recovery requires its original records and an atomic recovery publisher".into());
    }
    let existing = match &ledger.pre.worktree {
        GitWorktreeIdentity::File(file) => Some(
            repo.read_blobs(&[file.oid.clone()])
                .map_err(|error| error.to_string())?
                .remove(&file.oid)
                .ok_or("Missing legacy agreement pre-image")?,
        ),
        GitWorktreeIdentity::Missing => None,
    };
    let rendered = render_agreements_content(
        existing.as_deref(),
        batch.agreements.as_deref().unwrap_or_default(),
        policy,
    )
    .map_err(|error| error.to_string())?;
    let content = rendered
        .content
        .or(existing)
        .ok_or("Legacy agreement recovery has no ledger content")?;
    let mut effective = recovery.clone();
    effective.agreement_event_version = Some(1);
    let ledger = effective
        .paths
        .iter_mut()
        .find(|entry| entry.path == AGREEMENTS_PATH)
        .ok_or("Missing agreement path")?;
    let oid = repo.path_state.hash_index_blob(AGREEMENTS_PATH, &content, true).map_err(|error| error.to_string())?;
    ledger.post.index.as_mut().ok_or("Missing agreement post-image index")?.oid = oid;
    if let GitWorktreeIdentity::File(file) = &mut ledger.post.worktree {
        file.oid = repo
            .path_state
            .hash_worktree_blob(&content, true)
            .map_err(|error| error.to_string())?;
    }
    Ok(effective)
}
