//! Persisted replay cases ported from facts/agreements-replay.test.ts.
use crate::agreements::*;
use crate::facts::agreements_target::render_agreements_target;
use crate::memfs::parse_memory_file;
use serde_json::json;

const TEXT: &str = "Run the relevant tests before reporting completion";
const DATE: &str = "2026-10-01";
fn record(message: &str) -> AgreementExtractionRecord {
    parse_agreement_record(
        &json!({"action":"new","applies":"global","text":TEXT,"date":DATE,
        "source":{"sessionId":"session-a","messageId":message}}),
        0,
    )
    .unwrap()
}
fn changed(
    mut record: AgreementExtractionRecord,
    text: Option<&str>,
    date: Option<&str>,
) -> AgreementExtractionRecord {
    if let AgreementExtractionRecord::New {
        text: t, date: d, ..
    } = &mut record
    {
        if let Some(text) = text {
            *t = text.to_string();
        }
        if let Some(date) = date {
            *d = date.to_string();
        }
    }
    record
}
fn apply(
    root: &std::path::Path,
    records: &[AgreementExtractionRecord],
    cap: usize,
) -> Vec<AgreementItem> {
    let target = render_agreements_target(
        root,
        records,
        &AgreementsPolicy {
            max_items: cap,
            ..DEFAULT_AGREEMENTS_POLICY
        },
    )
    .unwrap();
    if let Some(content) = target.content {
        std::fs::write(root.join(AGREEMENTS_PATH), content).unwrap();
    }
    agreement_items_of(Some(
        &std::fs::read_to_string(root.join(AGREEMENTS_PATH)).unwrap(),
    ))
}
fn fixture() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("system")).unwrap();
    root
}
fn content(root: &std::path::Path) -> String {
    std::fs::read_to_string(root.join(AGREEMENTS_PATH)).unwrap()
}
fn receipts(content: &str) -> Vec<String> {
    parse_agreements_ledger(&parse_memory_file(content).unwrap().body).receipts
}

#[test]
fn one_source_counts_once_within_and_across_persisted_extractions() {
    let root = fixture();
    let root = root.path();
    assert_eq!(
        apply(root, &[record("turn-1"), record("turn-1")], 64)[0].n,
        1
    );
    let before = content(root);
    assert_eq!(apply(root, &[record("turn-1")], 64)[0].n, 1);
    assert_eq!(
        apply(
            root,
            &[changed(record("turn-1"), None, Some("2026-10-02"))],
            64
        )[0]
        .n,
        1
    );
    assert_eq!(content(root), before);
}

#[test]
fn distinct_turns_and_sessions_count_independently() {
    let root = fixture();
    let root = root.path();
    assert_eq!(
        apply(root, &[record("turn-1"), record("turn-2")], 64)[0].n,
        2
    );
    let mut next = record("turn-1");
    if let AgreementExtractionRecord::New {
        source: Some(source),
        ..
    } = &mut next
    {
        source.session_id = "session-b".to_string();
    }
    assert_eq!(apply(root, &[next], 64)[0].n, 3);
}

#[test]
fn legacy_dates_are_seeded_once_and_modern_dates_do_not_alias_them() {
    let root = fixture();
    let root = root.path();
    let mut legacy = record("unused");
    if let AgreementExtractionRecord::New { source, .. } = &mut legacy {
        *source = None;
    }
    assert_eq!(apply(root, &[legacy.clone()], 64)[0].n, 1);
    assert_eq!(apply(root, &[legacy.clone(), legacy.clone()], 64)[0].n, 1);
    assert_eq!(
        apply(
            root,
            &[changed(legacy.clone(), None, Some("2026-10-02"))],
            64
        )[0]
        .n,
        2
    );
    let root2 = fixture();
    let root2 = root2.path();
    assert_eq!(apply(root2, &[record("modern")], 64)[0].n, 1);
    assert_eq!(apply(root2, &[legacy.clone()], 64)[0].n, 2);
    assert_eq!(apply(root2, &[legacy], 64)[0].n, 2);
}

#[test]
fn legacy_recovery_migrates_original_last_before_modern_correction() {
    let root = fixture();
    let root = root.path();
    let item = AgreementItem {
        id: agreement_id(TEXT),
        scope: AgreementScope::Global,
        by: AgreementAuthor::Agent,
        text: TEXT.to_string(),
        n: 1,
        since: Some(DATE.to_string()),
        last: Some(DATE.to_string()),
        status: AgreementStatus::Active,
    };
    std::fs::write(
        root.join(AGREEMENTS_PATH),
        render_agreements_file(None, std::slice::from_ref(&item)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        apply(
            root,
            &[changed(record("modern"), None, Some("2026-10-02"))],
            64
        )[0]
        .n,
        2
    );
    let reaffirm = AgreementExtractionRecord::Reaffirm {
        id: item.id.clone(),
        date: DATE.to_string(),
        text: None,
        source: None,
    };
    assert_eq!(apply(root, &[reaffirm], 64)[0].n, 2);
    assert!(receipts(&content(root)).contains(&legacy_agreement_event_key(&item.id, DATE)));
}

#[test]
fn ack_and_drop_keep_receipts_and_allow_a_future_turn() {
    let root = fixture();
    let root = root.path();
    let items = apply(
        root,
        &[
            record("turn-1"),
            changed(record("turn-2"), None, Some("2026-10-02")),
        ],
        64,
    );
    assert_eq!(receipts(&content(root)).len(), 2);
    let acked = ack_agreement(&items, &items[0].id).unwrap();
    std::fs::write(
        root.join(AGREEMENTS_PATH),
        render_agreements_file(Some(&content(root)), &acked).unwrap(),
    )
    .unwrap();
    assert_eq!(receipts(&content(root)).len(), 2);
    assert_eq!(apply(root, &[record("turn-1")], 64)[0].n, 0);
    std::fs::write(
        root.join(AGREEMENTS_PATH),
        render_agreements_file(Some(&content(root)), &[]).unwrap(),
    )
    .unwrap();
    assert!(apply(root, &[record("turn-1")], 64).is_empty());
    assert_eq!(apply(root, &[record("turn-3")], 64)[0].n, 1);
}

#[test]
fn receipt_only_ledgers_cannot_be_deleted_or_edited_by_tools() {
    let root = fixture();
    let root = root.path();
    apply(root, &[record("turn-1")], 64);
    let dropped = render_agreements_file(Some(&content(root)), &[]).unwrap();
    let empty = "---\ndescription: Agreements\n---\n";
    let forged = format!("<!-- agreement-event: {} -->", "a".repeat(64));
    let old = serialize_agreement_events(&receipts(&dropped))[0].clone();
    for after in [
        None,
        Some(empty.to_string()),
        Some(format!("{dropped}{forged}\n")),
        Some(dropped.replace(&old, &forged)),
    ] {
        assert!(describe_agreements_tool_violation(Some(&dropped), after.as_deref()).is_some());
    }
    assert!(
        describe_agreements_tool_violation(None, Some(&format!("{empty}{forged}\n"))).is_some()
    );
    assert!(describe_agreements_tool_violation(Some(&dropped), Some(&dropped)).is_none());
}

#[test]
fn eviction_retains_receipts_without_recreating_the_old_item() {
    let root = fixture();
    let root = root.path();
    apply(root, &[record("turn-1")], 1);
    let next = changed(record("turn-2"), Some("Keep the fixture isolated"), None);
    apply(root, &[next], 1);
    let before = content(root);
    let target = render_agreements_target(
        root,
        &[record("turn-1")],
        &AgreementsPolicy {
            max_items: 1,
            ..DEFAULT_AGREEMENTS_POLICY
        },
    )
    .unwrap();
    assert!(target.content.is_none());
    assert_eq!(content(root), before);
}

#[test]
fn reworded_new_record_adds_alias_and_survives_drop() {
    let root = fixture();
    let root = root.path();
    let items = apply(root, &[record("turn-1")], 64);
    let text = "Run every relevant test before reporting completion";
    let reaffirm = AgreementExtractionRecord::Reaffirm {
        id: items[0].id.clone(),
        text: Some(text.to_string()),
        date: "2026-10-02".to_string(),
        source: Some(AgreementSourceEvent {
            session_id: "session-a".to_string(),
            message_id: "turn-2".to_string(),
        }),
    };
    apply(root, &[reaffirm], 64);
    let replay = changed(record("turn-3"), Some(text), Some("2026-10-03"));
    assert_eq!(apply(root, std::slice::from_ref(&replay), 64)[0].n, 3);
    assert_eq!(receipts(&content(root)).len(), 4);
    std::fs::write(
        root.join(AGREEMENTS_PATH),
        render_agreements_file(Some(&content(root)), &[]).unwrap(),
    )
    .unwrap();
    assert!(apply(root, &[replay], 64).is_empty());
}

#[test]
fn replay_alias_learned_before_cap_eviction_survives_the_batch() {
    let root = fixture();
    let root = root.path();
    let other = changed(
        record("other"),
        Some("Keep the fixture isolated"),
        Some("2026-10-02"),
    );
    let mut items = apply(root, &[record("turn-1"), other], 2);
    let text = "Run every relevant test before reporting completion";
    for item in &mut items {
        if item.text == TEXT {
            item.text = text.to_string();
        }
    }
    std::fs::write(
        root.join(AGREEMENTS_PATH),
        render_agreements_file(Some(&content(root)), &items).unwrap(),
    )
    .unwrap();
    let replay = changed(record("turn-1"), Some(text), None);
    let fresh = changed(
        record("fresh"),
        Some("Preserve fixture output"),
        Some("2026-10-03"),
    );
    let result = apply(root, &[replay.clone(), fresh, replay], 2);
    let mut texts: Vec<&str> = result.iter().map(|item| item.text.as_str()).collect();
    texts.sort();
    assert_eq!(
        texts,
        vec!["Keep the fixture isolated", "Preserve fixture output"]
    );
}

#[test]
fn source_identity_validation_excludes_context_and_non_user_messages() {
    let sourced = record("user-1");
    let payload = json!({"agreementEventVersion":1,"entries":[{"sessionId":"session-a","entries":[
        {"kind":"user","source_message_id":"user-1"}, {"kind":"assistant","source_message_id":"assistant-1"}
    ]}],"leadContext":[{"sessionId":"session-a","entries":[{"kind":"user","source_message_id":"lead-1"}]}]});
    assert!(validate_agreement_source(&sourced, &payload, 0).is_ok());
    for message in ["assistant-1", "lead-1", "missing"] {
        assert!(validate_agreement_source(&record(message), &payload, 0).is_err());
    }
    let mut legacy = sourced;
    if let AgreementExtractionRecord::New { source, .. } = &mut legacy {
        *source = None;
    }
    assert!(validate_agreement_source(&legacy, &payload, 0).is_err());
    assert!(validate_agreement_source(&legacy, &json!({"entries":[]}), 0).is_ok());
    for source in [
        json!(null),
        json!([]),
        json!({"sessionId":"s"}),
        json!({"sessionId":"s","messageId":" "}),
        json!({"sessionId":"s","messageId":"m","extra":true}),
    ] {
        assert!(
            parse_agreement_record(
                &json!({"action":"new","applies":"global","text":TEXT,"date":DATE,"source":source}),
                0
            )
            .is_err()
        );
    }
    assert_eq!(
        serde_json::to_value(record("user-1")).unwrap()["source"],
        json!({"sessionId":"session-a","messageId":"user-1"})
    );
}

#[test]
fn rendering_stamps_provenance_on_edits_and_drops_legacy_headers() {
    let modern = "---\ndescription: Agreements\nprovenance: \"user replied yes; a record, not the user's approval\"\n---\n";
    assert!(
        render_agreements_file(Some(modern), &[])
            .unwrap()
            .contains("user replied")
    );
    let mutation = apply_agreement_records(&[], &[record("turn-1")], &DEFAULT_AGREEMENTS_POLICY);
    assert!(
        render_agreements_file_with_receipts(
            Some(modern),
            &mutation.items,
            Some(&mutation.receipts)
        )
        .unwrap()
        .contains("mixed: parts")
    );
    let legacy = modern.replace(
        "user replied yes; a record, not the user's approval",
        "user said yes",
    );
    assert!(
        !render_agreements_file(Some(&legacy), &[])
            .unwrap()
            .contains("provenance:")
    );
}

#[test]
fn independent_committed_batches_replay_without_another_commit() {
    use crate::facts::extraction::{
        ApplyFactsBatchOptions, ApplyFactsBatchResult, FactsBatch, apply_facts_batch,
    };
    let root = tempfile::tempdir().unwrap();
    let repo = crate::support::test_repo::init_test_repo(root.path());
    let author = crate::git::GitCommitAuthor {
        agent_id: "fixture".to_string(),
        author_name: "Fixture".to_string(),
        author_email: None,
    };
    let batch = |id: &str| FactsBatch {
        batch_id: id.to_string(),
        records: vec![],
        agreements: Some(vec![record("turn-1")]),
    };
    let first = apply_facts_batch(
        &repo,
        batch("00000000-0000-4000-8000-000000000001"),
        &author,
        ApplyFactsBatchOptions::default(),
    )
    .unwrap();
    assert!(matches!(first, ApplyFactsBatchResult::Committed { .. }));
    let head = repo.head().unwrap();
    let second = apply_facts_batch(
        &repo,
        batch("00000000-0000-4000-8000-000000000002"),
        &author,
        ApplyFactsBatchOptions::default(),
    )
    .unwrap();
    assert!(matches!(second, ApplyFactsBatchResult::NoFacts { .. }));
    assert_eq!(repo.head().unwrap(), head);
    assert_eq!(agreement_items_of(Some(&content(root.path())))[0].n, 1);
    assert!(repo.status(&[] as &[&str]).unwrap().trim().is_empty());
}
