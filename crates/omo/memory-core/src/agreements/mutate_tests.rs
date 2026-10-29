use pretty_assertions::assert_eq;

use super::{
    AgreementEvent, ack_agreement, add_human_agreement, apply_agreement_records, drop_agreement,
};
use crate::agreements::format::agreement_id;
use crate::agreements::types::{
    AgreementAuthor, AgreementExtractionRecord, AgreementItem, AgreementScope, AgreementStatus,
    AgreementsPolicy, DEFAULT_AGREEMENTS_POLICY,
};

fn new_record(text: &str, date: &str) -> AgreementExtractionRecord {
    AgreementExtractionRecord::New {
        applies: AgreementScope::Global,
        text: text.to_string(),
        date: date.to_string(),
        source: None,
    }
}

fn agent(text: &str, n: u64, date: &str) -> AgreementItem {
    AgreementItem {
        id: agreement_id(text),
        scope: AgreementScope::Global,
        by: AgreementAuthor::Agent,
        text: text.to_string(),
        n,
        since: Some(date.to_string()),
        last: Some(date.to_string()),
        status: AgreementStatus::Active,
    }
}

#[test]
fn new_record_creates_an_agent_item_with_count_one() {
    let out = apply_agreement_records(
        &[],
        &[new_record("Run tests", "2026-01-01")],
        &DEFAULT_AGREEMENTS_POLICY,
    );
    assert_eq!(out.items.len(), 1);
    assert_eq!(out.items[0].n, 1);
    assert_eq!(out.items[0].by, AgreementAuthor::Agent);
    assert!(out.newly_flagged.is_empty());
}

#[test]
fn repeat_bumps_count_and_flags_at_threshold_once() {
    let existing = [agent("Run tests", 2, "2026-01-01")];
    let out = apply_agreement_records(
        &existing,
        &[
            new_record("run tests.", "2026-02-01"),
            new_record("Run tests", "2026-03-01"),
        ],
        &DEFAULT_AGREEMENTS_POLICY,
    );
    assert_eq!(out.items[0].n, 4);
    assert_eq!(out.items[0].status, AgreementStatus::NeedsHuman);
    assert_eq!(out.items[0].last.as_deref(), Some("2026-03-01"));
    assert_eq!(out.newly_flagged, vec![existing[0].id.clone()]);
}

#[test]
fn reaffirm_rewords_agent_items_only_and_stale_ids_are_events() {
    let mut human = agent("Never push", 0, "2026-01-01");
    human.by = AgreementAuthor::Human;
    let existing = [human.clone(), agent("Be brief", 0, "2026-01-01")];
    let out = apply_agreement_records(
        &existing,
        &[
            AgreementExtractionRecord::Reaffirm {
                id: human.id.clone(),
                date: "2026-02-01".to_string(),
                text: Some("changed".to_string()),
                source: None,
            },
            AgreementExtractionRecord::Reaffirm {
                id: existing[1].id.clone(),
                date: "2026-02-01".to_string(),
                text: Some("Be very brief".to_string()),
                source: None,
            },
            AgreementExtractionRecord::Reaffirm {
                id: "ffffffffff".to_string(),
                date: "2026-02-02".to_string(),
                text: None,
                source: None,
            },
        ],
        &DEFAULT_AGREEMENTS_POLICY,
    );
    let texts: Vec<&str> = out.items.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(texts, ["Never push", "Be very brief"]);
    assert_eq!(
        out.events,
        vec![AgreementEvent::StaleReaffirm {
            date: "2026-02-02".to_string(),
            id: "ffffffffff".to_string()
        }]
    );
}

#[test]
fn cap_evicts_oldest_committed_uncounted_agent_item() {
    let policy = AgreementsPolicy {
        needs_human_threshold: 3,
        max_items: 2,
    };
    let existing = [
        agent("Old", 1, "2026-01-01"),
        agent("Newer", 1, "2026-02-01"),
    ];
    let out = apply_agreement_records(&existing, &[new_record("Fresh", "2026-03-01")], &policy);
    let texts: Vec<&str> = out.items.iter().map(|item| item.text.as_str()).collect();
    assert_eq!(texts, ["Newer", "Fresh"]);
    assert!(matches!(
        out.events.as_slice(),
        [AgreementEvent::Evicted { item, .. }] if item.text == "Old"
    ));
}

#[test]
fn cap_refuses_when_nothing_is_evictable_and_never_evicts_this_batch() {
    let policy = AgreementsPolicy {
        needs_human_threshold: 3,
        max_items: 1,
    };
    let existing = [agent("Counted", 2, "2026-01-01")];
    let out = apply_agreement_records(&existing, &[new_record("Fresh", "2026-03-01")], &policy);
    assert_eq!(out.items.len(), 1);
    assert_eq!(
        out.events,
        vec![AgreementEvent::Refused {
            date: "2026-03-01".to_string(),
            text: "Fresh".to_string()
        }]
    );
    let empty = apply_agreement_records(
        &[],
        &[
            new_record("One", "2026-03-01"),
            new_record("Two", "2026-03-02"),
        ],
        &policy,
    );
    assert_eq!(empty.items.len(), 1);
    assert!(matches!(
        empty.events.as_slice(),
        [AgreementEvent::Refused { .. }]
    ));
}

#[test]
fn human_add_ack_and_drop() {
    let existing = vec![agent("Be brief", 3, "2026-01-01")];
    let added = add_human_agreement(&existing, "be brief", AgreementScope::Project, "2026-05-05");
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].by, AgreementAuthor::Human);
    assert_eq!(added[0].n, 3);
    assert_eq!(added[0].scope, AgreementScope::Project);

    let fresh = add_human_agreement(&[], "Ask first", AgreementScope::Global, "2026-05-05");
    assert_eq!(
        (fresh[0].n, fresh[0].since.as_deref()),
        (0, Some("2026-05-05"))
    );

    let mut flagged = existing[0].clone();
    flagged.status = AgreementStatus::NeedsHuman;
    let acked = ack_agreement(&[flagged.clone()], &flagged.id).expect("ack");
    assert_eq!((acked[0].n, acked[0].status), (0, AgreementStatus::Active));
    assert_eq!(ack_agreement(&[flagged.clone()], "ffffffffff"), None);
    assert_eq!(
        drop_agreement(&[flagged.clone()], &flagged.id),
        Some(vec![])
    );
    assert_eq!(drop_agreement(&[flagged], "ffffffffff"), None);
}
