//! Renders the agreements ledger a facts batch would leave behind.
use super::mutation_plan::MutationPlanError;
use crate::agreements::events::{parse_agreement_event_line, serialize_agreement_events};
use crate::agreements::{
    AgreementEvent, AgreementExtractionRecord, AgreementsPolicy, agreement_items_of,
    read_agreements_worktree, render_agreements_file,
};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgreementsTarget {
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
    let prior: Vec<String> = existing
        .into_iter()
        .flat_map(str::lines)
        .filter_map(parse_agreement_event_line)
        .collect();
    let mutation = crate::agreements::mutate::apply_agreement_records_with_receipts(
        &agreement_items_of(existing),
        records,
        policy,
        &prior,
    );
    // Receipt comments are parsed separately from the opaque ledger lines and serialized once.
    let clean = existing.map(|text| {
        text.split_inclusive('\n')
            .filter(|line| {
                parse_agreement_event_line(line.trim_end_matches(['\n', '\r'])).is_none()
            })
            .collect::<String>()
    });
    let mut content = render_agreements_file(clean.as_deref(), &mutation.items)?;
    for line in serialize_agreement_events(&mutation.receipts) {
        content.push_str(&line);
        content.push('\n');
    }
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
    use crate::agreements::{AgreementScope, types::AgreementSource};
    #[test]
    fn same_source_is_idempotent_across_date_changes_and_drop() {
        let source = AgreementSource {
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
