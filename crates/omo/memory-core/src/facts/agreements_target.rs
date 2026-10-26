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

pub fn render_agreements_content(
    existing: Option<&str>,
    records: &[AgreementExtractionRecord],
    policy: &AgreementsPolicy,
) -> Result<AgreementsTarget, MutationPlanError> {
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
    let content =
        render_agreements_file_with_receipts(existing, &mutation.items, Some(&mutation.receipts))?;
    Ok(AgreementsTarget {
        content: (existing != Some(content.as_str())).then_some(content),
        events: mutation.events,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duplicate_receipts_are_byte_stable_across_replay() {
        let records = vec![AgreementExtractionRecord::New {
            applies: crate::agreements::AgreementScope::Global,
            text: "Run tests".into(),
            date: "2026-10-01".into(),
            source: None,
        }];
        let first = render_agreements_content(None, &records, &AgreementsPolicy::default())
            .unwrap()
            .content
            .unwrap();
        assert!(
            render_agreements_content(Some(&first), &records, &AgreementsPolicy::default())
                .unwrap()
                .content
                .is_none()
        );
        assert_eq!(agreement_items_of(Some(&first))[0].n, 1);
    }
}

#[cfg(test)]
mod replay_tests {
    use super::*;
    use crate::agreements::{AgreementScope, types::AgreementSourceEvent};
    #[test]
    fn same_source_is_idempotent_across_date_changes_and_drop() {
        let source = AgreementSourceEvent {
            session_id: "s".into(),
            message_id: "m".into(),
        };
        let new = |date: &str| AgreementExtractionRecord::New {
            applies: AgreementScope::Global,
            text: "Run tests".into(),
            date: date.into(),
            source: Some(source.clone()),
        };
        let first = render_agreements_content(None, &[new("2026-10-01")], &Default::default())
            .unwrap()
            .content
            .unwrap();
        assert!(
            render_agreements_content(Some(&first), &[new("2026-10-02")], &Default::default())
                .unwrap()
                .content
                .is_none()
        );
        let dropped = crate::agreements::render_agreements_file(Some(&first), &[]).unwrap();
        let replay =
            render_agreements_content(Some(&dropped), &[new("2026-10-03")], &Default::default())
                .unwrap();
        assert!(replay.content.is_none());
    }
}
