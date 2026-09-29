use pretty_assertions::assert_eq;
use tempfile::tempdir;

use crate::facts::extraction::{FactsBatch, FactsExtractionRecord};
use crate::facts::mutation_plan::{
    MutationPlanError, facts_records_hash, plan_facts_mutation, render_facts_note,
};

fn sample_records() -> Vec<FactsExtractionRecord> {
    vec![
        FactsExtractionRecord::Project {
            text: "Uses Bun.".to_string(),
            date: "2026-08-10".to_string(),
        },
        FactsExtractionRecord::Project {
            text: "Uses Rust.".to_string(),
            date: "2026-08-11".to_string(),
        },
    ]
}

#[test]
fn test_facts_records_hash_deterministic() {
    // given
    let records1 = sample_records();
    let records2 = sample_records();
    let mut records3 = sample_records();
    records3.reverse();

    // when
    let hash1 = facts_records_hash(&records1);
    let hash2 = facts_records_hash(&records2);
    let hash3 = facts_records_hash(&records3);

    // then
    assert_eq!(hash1.len(), 64);
    assert_eq!(hash1, hash2);
    assert_ne!(hash1, hash3);
}

#[test]
fn test_render_facts_note_fresh() {
    // given
    let dir = tempdir().expect("tempdir");
    let records = sample_records();

    // when
    let rendered = render_facts_note(dir.path(), "notes/facts/2026-08.md", &records)
        .expect("render fresh note");

    // then
    assert!(rendered.contains("description: Explicit facts for 2026-08"));
    assert!(rendered.contains("- [2026-08-10] Uses Bun."));
    assert!(rendered.contains("- [2026-08-11] Uses Rust."));
}

#[test]
fn test_render_facts_note_existing() {
    // given
    let dir = tempdir().expect("tempdir");
    let file_path = dir.path().join("notes/facts/2026-08.md");
    std::fs::create_dir_all(file_path.parent().unwrap()).expect("create dirs");
    std::fs::write(
        &file_path,
        "---\ndescription: Existing note\n---\nInitial fact line.\n",
    )
    .expect("write existing");
    let records = sample_records();

    // when
    let rendered = render_facts_note(dir.path(), "notes/facts/2026-08.md", &records)
        .expect("render existing note");

    // then
    assert!(rendered.contains("description: Existing note"));
    assert!(rendered.contains("Initial fact line."));
    assert!(rendered.contains("- [2026-08-10] Uses Bun."));
}

#[test]
fn test_plan_facts_mutation_clean_repo() {
    // given
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    let batch = FactsBatch {
        batch_id: "00000000-0000-4000-8000-000000000001".to_string(),
        records: sample_records(),
    };

    // when
    let recovery = plan_facts_mutation(&repo, &batch, None, None).expect("plan mutation");

    // then
    assert_eq!(recovery.version, 1);
    assert_eq!(recovery.batch_id, batch.batch_id);
    assert_eq!(recovery.paths.len(), 1);
    assert_eq!(recovery.paths[0].path, "notes/facts/2026-08.md");
}

#[test]
fn test_plan_facts_mutation_parent_dirty() {
    // given
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    let uncommitted_file = dir.path().join("uncommitted.txt");
    std::fs::write(&uncommitted_file, "dirty\n").expect("write dirty file");
    let batch = FactsBatch {
        batch_id: "00000000-0000-4000-8000-000000000002".to_string(),
        records: sample_records(),
    };

    // when
    let result = plan_facts_mutation(&repo, &batch, None, None);

    // then
    assert!(matches!(result, Err(MutationPlanError::ParentDirty(_))));
}
