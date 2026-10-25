//! Renders the agreements ledger a facts batch would leave behind.

use std::path::Path;

use crate::agreements::{
    AgreementEvent, AgreementExtractionRecord, AgreementsPolicy, agreement_items_of,
    apply_agreement_records_with_receipts, parse_agreements_ledger, read_agreements_worktree,
    render_agreements_file_with_receipts,
};

use super::mutation_plan::MutationPlanError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgreementsTarget {
    /// The rendered ledger, or `None` when the records leave it byte-identical (stale reaffirms, refusals).
    pub content: Option<String>,
    pub events: Vec<AgreementEvent>,
}

pub fn render_agreements_target(
    repo_dir: &Path,
    records: &[AgreementExtractionRecord],
    policy: &AgreementsPolicy,
) -> Result<AgreementsTarget, MutationPlanError> {
    let existing = read_agreements_worktree(repo_dir)?;
    render_agreements_content(existing.as_deref(), records, policy)
}

pub fn render_agreements_content(existing: Option<&str>, records: &[AgreementExtractionRecord], policy: &AgreementsPolicy) -> Result<AgreementsTarget, MutationPlanError> {
    let prior = existing
        .map(crate::memfs::parse_memory_file)
        .transpose()?
        .map(|file| parse_agreements_ledger(&file.body).receipts)
        .unwrap_or_default();
    let mutation = apply_agreement_records_with_receipts(
        &agreement_items_of(existing),
        records,
        policy,
        &prior,
    );
    let content = render_agreements_file_with_receipts(
        existing,
        &mutation.items,
        Some(&mutation.receipts),
    )?;
    Ok(AgreementsTarget {
        content: (existing != Some(content.as_str())).then_some(content),
        events: mutation.events,
    })
}
