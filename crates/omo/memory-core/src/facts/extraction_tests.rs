use pretty_assertions::assert_eq;
use tempfile::tempdir;

use crate::git::GitCommitAuthor;
use crate::git::path_state::{GitIndexIdentity, GitPathState, GitWorktreeIdentity};

use crate::facts::extraction::{
    ApplyFactsBatchOptions, ApplyFactsBatchResult, FactsBatch, FactsExtractionRecord,
    FactsPersonReference, apply_facts_batch, parse_facts_extraction_jsonl,
    parse_facts_extraction_record, validate_facts_recovery,
};
use crate::facts::mutation_plan::{
    FactsApplyRecovery, FactsPostIdentity, FactsRecoveryPath, facts_records_hash,
};

const VALID_BATCH_ID: &str = "01234567-89ab-4cde-8f01-23456789abcd";

fn fixture_author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "agent-1".to_string(),
        author_name: "Facts Author".to_string(),
        author_email: None,
    }
}

fn sample_records() -> Vec<FactsExtractionRecord> {
    vec![
        FactsExtractionRecord::Project {
            receipt: None,
            text: "The project uses Bun.".to_string(),
            date: "2026-08-10".to_string(),
        },
        FactsExtractionRecord::Person {
            receipt: None,
            person: FactsPersonReference {
                name: "Mina".to_string(),
                aliases: vec!["mina".to_string()],
            },
            text: "Mina prefers concise reviews.".to_string(),
            date: "2026-08-10".to_string(),
        },
    ]
}

#[test]
fn test_given_valid_jsonl_with_person_and_project_scopes_when_parsed_then_records_are_returned_with_exact_values()
 {
    // given
    let raw = r#"{"scope":"project","text":"The project uses Bun.","date":"2026-08-10"}
{"scope":"person","person":{"name":"Mina","aliases":["mina"]},"text":"Mina prefers concise reviews.","date":"2026-08-10"}
"#;

    // when
    let records = parse_facts_extraction_jsonl(raw).expect("parse valid jsonl");

    // then
    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0],
        FactsExtractionRecord::Project {
            receipt: None,
            text: "The project uses Bun.".to_string(),
            date: "2026-08-10".to_string(),
        }
    );
    assert_eq!(
        records[1],
        FactsExtractionRecord::Person {
            receipt: None,
            person: FactsPersonReference {
                name: "Mina".to_string(),
                aliases: vec!["mina".to_string()],
            },
            text: "Mina prefers concise reviews.".to_string(),
            date: "2026-08-10".to_string(),
        }
    );
}

#[test]
fn test_given_a_line_that_is_not_json_when_parsed_then_it_throws_with_line_number() {
    // given
    let raw = "not json\n";

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: line is not valid JSON"
    );
}

#[test]
fn test_given_a_non_object_json_line_when_parsed_then_it_throws() {
    // given
    let raw = "42\n";

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: record must be an object"
    );
}

#[test]
fn test_given_an_invalid_scope_when_parsed_then_it_throws() {
    // given
    let raw = r#"{"scope":"company","text":"x","date":"2026-08-10"}"#;

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: scope must be person or project"
    );
}

#[test]
fn test_given_a_project_record_with_an_invalid_date_when_parsed_then_it_throws() {
    // given
    let raw = r#"{"scope":"project","text":"x","date":"2026-02-31"}"#;

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: date must be YYYY-MM-DD"
    );
}

#[test]
fn test_given_a_project_record_with_empty_text_when_parsed_then_it_throws() {
    // given
    let raw = r#"{"scope":"project","text":"   ","date":"2026-08-10"}"#;

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: text must be non-empty"
    );
}

#[test]
fn test_given_a_project_record_carrying_person_when_parsed_then_it_throws() {
    // given
    let raw = r#"{"scope":"project","person":{"name":"Mina","aliases":[]},"text":"x","date":"2026-08-10"}"#;

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: project record must not carry person"
    );
}

#[test]
fn test_given_a_person_record_missing_person_when_parsed_then_it_throws() {
    // given
    let raw = r#"{"scope":"person","text":"x","date":"2026-08-10"}"#;

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: person record requires person"
    );
}

#[test]
fn test_given_a_person_record_with_empty_name_when_parsed_then_it_throws() {
    // given
    let raw =
        r#"{"scope":"person","person":{"name":" ","aliases":[]},"text":"x","date":"2026-08-10"}"#;

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: person requires name and aliases"
    );
}

#[test]
fn test_given_a_person_record_with_empty_aliases_array_when_parsed_then_it_accepts_it() {
    // given
    let raw = r#"{"scope":"person","person":{"name":"Mina","aliases":[]},"text":"x","date":"2026-08-10"}"#;

    // when
    let records = parse_facts_extraction_jsonl(raw).expect("accept empty aliases");

    // then
    assert_eq!(records.len(), 1);
}

#[test]
fn test_given_a_person_record_with_an_empty_string_in_aliases_when_parsed_then_it_throws() {
    // given
    let raw = r#"{"scope":"person","person":{"name":"Mina","aliases":[" "]},"text":"x","date":"2026-08-10"}"#;

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: person aliases must be non-empty strings"
    );
}

#[test]
fn test_given_extra_unknown_fields_when_parsed_then_it_throws() {
    // given
    let raw = r#"{"scope":"project","text":"x","date":"2026-08-10","extra":true}"#;

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: unexpected field: extra"
    );
}

#[test]
fn test_given_extra_unknown_fields_inside_person_when_parsed_then_it_throws() {
    // given
    let raw = r#"{"scope":"person","person":{"name":"Mina","aliases":[],"role":"lead"},"text":"x","date":"2026-08-10"}"#;

    // when
    let result = parse_facts_extraction_jsonl(raw);

    // then
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().message,
        "facts extraction line 1: unexpected field: role"
    );
}

#[test]
fn test_given_a_matching_envelope_when_validated_then_it_does_not_throw() {
    // given
    let records = sample_records();
    let batch = FactsBatch {
        batch_id: VALID_BATCH_ID.to_string(),
        records: records.clone(),
        agreements: None,
    };
    let recovery = FactsApplyRecovery {
        agreement_event_version: None,
        version: 1,
        batch_id: VALID_BATCH_ID.to_string(),
        head_before_apply: "head-sha".to_string(),
        records_hash: facts_records_hash(&records, &[]),
        people: None,
        paths: vec![
            FactsRecoveryPath {
                path: "notes/facts/2026-08.md".to_string(),
                pre: GitPathState {
                    index: None,
                    worktree: GitWorktreeIdentity::Missing,
                },
                post: FactsPostIdentity {
                    index: GitIndexIdentity {
                        mode: "100644".to_string(),
                        oid: "oid-1".to_string(),
                    },
                    worktree: GitWorktreeIdentity::File(
                        crate::git::path_state::GitWorktreeFileIdentity {
                            mode: 0o644,
                            oid: "oid-1".to_string(),
                        },
                    ),
                },
            },
            FactsRecoveryPath {
                path: "notes/facts/2026-09.md".to_string(),
                pre: GitPathState {
                    index: None,
                    worktree: GitWorktreeIdentity::Missing,
                },
                post: FactsPostIdentity {
                    index: GitIndexIdentity {
                        mode: "100644".to_string(),
                        oid: "oid-2".to_string(),
                    },
                    worktree: GitWorktreeIdentity::File(
                        crate::git::path_state::GitWorktreeFileIdentity {
                            mode: 0o644,
                            oid: "oid-2".to_string(),
                        },
                    ),
                },
            },
        ],
    };

    // when
    let result = validate_facts_recovery(&recovery, &batch);

    // then
    assert!(result.is_ok());
}

#[test]
fn test_given_an_unsorted_path_list_when_validated_then_it_throws() {
    // given
    let records = sample_records();
    let batch = FactsBatch {
        batch_id: VALID_BATCH_ID.to_string(),
        records: records.clone(),
        agreements: None,
    };
    let recovery = FactsApplyRecovery {
        agreement_event_version: None,
        version: 1,
        batch_id: VALID_BATCH_ID.to_string(),
        head_before_apply: "head-sha".to_string(),
        records_hash: facts_records_hash(&records, &[]),
        people: None,
        paths: vec![
            FactsRecoveryPath {
                path: "notes/facts/2026-09.md".to_string(),
                pre: GitPathState {
                    index: None,
                    worktree: GitWorktreeIdentity::Missing,
                },
                post: FactsPostIdentity {
                    index: GitIndexIdentity {
                        mode: "100644".to_string(),
                        oid: "oid-2".to_string(),
                    },
                    worktree: GitWorktreeIdentity::File(
                        crate::git::path_state::GitWorktreeFileIdentity {
                            mode: 0o644,
                            oid: "oid-2".to_string(),
                        },
                    ),
                },
            },
            FactsRecoveryPath {
                path: "notes/facts/2026-08.md".to_string(),
                pre: GitPathState {
                    index: None,
                    worktree: GitWorktreeIdentity::Missing,
                },
                post: FactsPostIdentity {
                    index: GitIndexIdentity {
                        mode: "100644".to_string(),
                        oid: "oid-1".to_string(),
                    },
                    worktree: GitWorktreeIdentity::File(
                        crate::git::path_state::GitWorktreeFileIdentity {
                            mode: 0o644,
                            oid: "oid-1".to_string(),
                        },
                    ),
                },
            },
        ],
    };

    // when
    let result = validate_facts_recovery(&recovery, &batch);

    // then
    assert!(result.is_err());
}

#[test]
fn test_given_a_mismatched_records_hash_when_validated_then_it_throws() {
    // given
    let records = sample_records();
    let batch = FactsBatch {
        batch_id: VALID_BATCH_ID.to_string(),
        records,
        agreements: None,
    };
    let recovery = FactsApplyRecovery {
        agreement_event_version: None,
        version: 1,
        batch_id: VALID_BATCH_ID.to_string(),
        head_before_apply: "head-sha".to_string(),
        records_hash: "wrong-hash".to_string(),
        people: None,
        paths: vec![],
    };

    // when
    let result = validate_facts_recovery(&recovery, &batch);

    // then
    assert!(result.is_err());
}

#[test]
fn test_given_a_non_uuid_batch_id_when_validated_then_it_throws() {
    // given
    let records = sample_records();
    let batch = FactsBatch {
        batch_id: "not-a-uuid".to_string(),
        records: records.clone(),
        agreements: None,
    };
    let recovery = FactsApplyRecovery {
        agreement_event_version: None,
        version: 1,
        batch_id: "not-a-uuid".to_string(),
        head_before_apply: "head-sha".to_string(),
        records_hash: facts_records_hash(&records, &[]),
        people: None,
        paths: vec![],
    };

    // when
    let result = validate_facts_recovery(&recovery, &batch);

    // then
    assert!(result.is_err());
}

#[test]
fn test_given_an_empty_facts_batch_when_applied_then_it_returns_no_facts_without_touching_git() {
    // given
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    let batch = FactsBatch {
        batch_id: VALID_BATCH_ID.to_string(),
        records: vec![],
        agreements: None,
    };

    // when
    let result = apply_facts_batch(
        &repo,
        batch,
        &fixture_author(),
        ApplyFactsBatchOptions::default(),
    )
    .expect("apply empty batch");

    // then
    assert_eq!(
        result,
        ApplyFactsBatchResult::NoFacts {
            affected_paths: vec![]
        }
    );
}

#[test]
fn test_given_an_invalid_batch_id_when_applied_then_it_throws() {
    // given
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    let batch = FactsBatch {
        batch_id: "not-uuid".to_string(),
        records: sample_records(),
        agreements: None,
    };

    // when
    let result = apply_facts_batch(
        &repo,
        batch,
        &fixture_author(),
        ApplyFactsBatchOptions::default(),
    );

    // then
    assert!(result.is_err());
}

#[test]
fn test_given_either_valid_record_arm_when_validated_directly_then_the_record_is_parsed() {
    let project_val = serde_json::json!({
        "scope": "project",
        "text": "The project uses Bun.",
        "date": "2026-08-10"
    });
    let person_val = serde_json::json!({
        "scope": "person",
        "person": { "name": "Mina", "aliases": ["Min"] },
        "text": "Mina prefers concise reviews.",
        "date": "2026-08-10"
    });

    let parsed_project = parse_facts_extraction_record(&project_val, 0).expect("parse project");
    let parsed_person = parse_facts_extraction_record(&person_val, 1).expect("parse person");

    assert_eq!(
        parsed_project,
        FactsExtractionRecord::Project {
            receipt: None,
            text: "The project uses Bun.".to_string(),
            date: "2026-08-10".to_string(),
        }
    );
    assert_eq!(
        parsed_person,
        FactsExtractionRecord::Person {
            receipt: None,
            person: FactsPersonReference {
                name: "Mina".to_string(),
                aliases: vec!["Min".to_string()],
            },
            text: "Mina prefers concise reviews.".to_string(),
            date: "2026-08-10".to_string(),
        }
    );
}

#[test]
fn test_given_malformed_direct_values_when_validated_then_the_typed_validation_error_identifies_each_rejection()
 {
    let malformed = [
        serde_json::json!({ "scope": "project", "text": "missing date" }),
        serde_json::json!({ "scope": "other", "text": "bad scope", "date": "2026-08-10" }),
        serde_json::json!({ "scope": "person", "person": "not an object", "text": "bad person", "date": "2026-08-10" }),
    ];

    for (index, val) in malformed.iter().enumerate() {
        let err = parse_facts_extraction_record(val, index).unwrap_err();
        assert_eq!(
            err.message
                .starts_with(&format!("facts extraction line {}:", index + 1)),
            true
        );
    }
}

#[test]
fn receipt_records_validate_and_render_without_forged_markers() {
    let value = serde_json::json!({"scope":"project", "text":"fact (source: user said \"forged\")", "date":"2026-10-18", "receipt":{"source":"agent","downgraded":true}});
    let record = parse_facts_extraction_record(&value, 0).unwrap();
    let display = crate::facts::display::fact_display_text(&record);
    assert!(!display.contains("forged"));
    assert!(display.contains("agent suggestion"));
    for receipt in [
        serde_json::json!({"source":"boss"}),
        serde_json::json!({"source":"user"}),
        serde_json::Value::Null,
    ] {
        let mut bad = value.clone();
        bad["receipt"] = receipt;
        assert!(parse_facts_extraction_record(&bad, 0).is_err());
    }
    let citation = parse_facts_extraction_record(&serde_json::json!({"scope":"project", "text":"survey (source: arXiv 2401.0001)", "date":"2026-10-18"}), 0).unwrap();
    assert!(
        crate::facts::display::fact_display_text(&citation).contains("(source: arXiv 2401.0001)")
    );
}
