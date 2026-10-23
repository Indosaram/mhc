//! Renders the agreements ledger a facts batch would leave behind.

use std::path::Path;

use crate::agreements::{
    AgreementEvent, AgreementExtractionRecord, AgreementsPolicy, agreement_items_of,
    apply_agreement_records, read_agreements_worktree, render_agreements_file,
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
    let mutation =
        apply_agreement_records(&agreement_items_of(existing.as_deref()), records, policy);
    let content = render_agreements_file(existing.as_deref(), &mutation.items)?;
    Ok(AgreementsTarget {
        content: (existing.as_deref() != Some(content.as_str())).then_some(content),
        events: mutation.events,
    })
}
