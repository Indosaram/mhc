use pretty_assertions::assert_eq;
use tempfile::{TempDir, tempdir};

use crate::facts::extraction::{FactsBatch, FactsExtractionRecord};
use crate::facts::mutation_plan::{
    MutationPlanError, facts_records_hash, plan_facts_mutation, render_facts_note,
};
use crate::git::{GitMemoryRepo, GitMemoryRepoOptions, GitSeedFile, InitializeGitRepoOptions};

fn sample_records() -> Vec<FactsExtractionRecord> {
    vec![
        FactsExtractionRecord::Project {
            receipt: None,
            text: "Uses Bun.".to_string(),
            date: "2026-08-10".to_string(),
        },
        FactsExtractionRecord::Project {
            receipt: None,
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
    let hash1 = facts_records_hash(&records1, &[]);
    let hash2 = facts_records_hash(&records2, &[]);
    let hash3 = facts_records_hash(&records3, &[]);

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
        agreements: None,
    };

    // when
    let recovery = plan_facts_mutation(&repo, &batch, None, None, &Default::default(), None)
        .expect("plan mutation");

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
        agreements: None,
    };

    // when
    let result = plan_facts_mutation(&repo, &batch, None, None, &Default::default(), None);

    // then
    assert!(matches!(result, Err(MutationPlanError::ParentDirty(_))));
}

#[test]
fn test_facts_records_hash_is_stable_without_agreements_and_changes_with_them() {
    use crate::agreements::{AgreementExtractionRecord, AgreementScope};

    // given
    let records = sample_records();
    let agreements = vec![AgreementExtractionRecord::New {
        applies: AgreementScope::Global,
        text: "Run tests".to_string(),
        date: "2026-08-10".to_string(),
        source: None,
    }];

    // when
    let plain = facts_records_hash(&records, &[]);
    let with_agreements = facts_records_hash(&records, &agreements);

    // then
    let legacy = crate::support::sha256::sha256_hex(
        serde_json::to_string(&records).expect("json").as_bytes(),
    );
    assert_eq!(plain, legacy);
    assert!(plain != with_agreements);
}

#[test]
fn test_plan_facts_mutation_never_reuses_archived_ledger_path() {
    let temp = TempDir::new().expect("temp");
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: temp.path().join("repo"),
        agent_id: "agent-one".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");
    repo.init(InitializeGitRepoOptions {
        seed_files: vec![
            GitSeedFile {
                relative_path: "system/persona.md".to_string(),
                content: "---\ndescription: Persona\n---\n\nbody\n".to_string(),
            },
            GitSeedFile {
                relative_path: "notes/facts/2026-08.md".to_string(),
                content: "---\ndescription: August 2026 facts\n---\n\narchived body\n".to_string(),
            },
        ],
        author_name: None,
        install_hooks: None,
    })
    .expect("init repo");
    std::fs::remove_file(repo.dir.join("notes/facts/2026-08.md")).expect("remove archived ledger");
    std::fs::write(
        repo.dir.join("ARCHIVE.md"),
        "---\ndescription: Archive index\n---\n\n- notes/facts/2026-08.md\n",
    )
    .expect("write archive");
    repo.commit_write(
        &["notes/facts/2026-08.md", "ARCHIVE.md"],
        "archive 2026-08 facts",
        &crate::git::repo_types::GitCommitAuthor {
            agent_id: "agent-one".to_owned(),
            author_name: "fixture".to_owned(),
            author_email: None,
        },
    )
    .expect("commit");
    let batch = FactsBatch {
        batch_id: "00000000-0000-4000-8000-000000000003".to_string(),
        records: sample_records(),
        agreements: None,
    };

    let recovery = plan_facts_mutation(&repo, &batch, None, None, &Default::default(), None)
        .expect("plan succeeds");
    assert_eq!(recovery.paths[0].path, "notes/facts/2026-08-2.md");
}
