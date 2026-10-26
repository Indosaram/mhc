use super::*;
use crate::agreements::{AgreementExtractionRecord, AgreementScope, agreement_items_of};
use crate::facts::{
    extraction::{ApplyFactsBatchOptions, apply_facts_batch},
    mutation_plan::plan_facts_mutation,
    recovery::{apply_facts_recovery, apply_facts_recovery_with_input},
};
use crate::git::GitCommitAuthor;

#[test]
fn legacy_recovery_restores_original_before_publishing_migration() {
    for partial in [false, true] {
        for (date, expected) in [("2026-10-01", 1), ("2026-10-02", 2)] {
            let dir = tempfile::tempdir().unwrap();
            let repo = crate::support::test_repo::init_test_repo(dir.path());
            let author = GitCommitAuthor {
                agent_id: "fixture".into(),
                author_name: "Fixture".into(),
                author_email: None,
            };
            let mut batch = FactsBatch {
                batch_id: "11111111-1111-4111-8111-111111111111".into(),
                records: vec![],
                agreements: Some(vec![AgreementExtractionRecord::New {
                    applies: AgreementScope::Global,
                    text: "Use the fixture convention".into(),
                    date: "2026-10-01".into(),
                    source: None,
                }]),
            };
            apply_facts_batch(
                &repo,
                batch.clone(),
                &author,
                ApplyFactsBatchOptions::default(),
            )
            .unwrap();
            // Upstream facts/agreement-recovery.test.ts starts from a pre-event ledger.
            let ledger = std::fs::read_to_string(dir.path().join(AGREEMENTS_PATH)).unwrap();
            let legacy = ledger
                .lines()
                .filter(|line| !line.starts_with("<!-- agreement-event: "))
                .collect::<Vec<_>>()
                .join("\n")
                + "\n";
            std::fs::write(dir.path().join(AGREEMENTS_PATH), legacy).unwrap();
            repo.commit_write(&[AGREEMENTS_PATH], "seed legacy ledger", &author)
                .unwrap();
            let id = agreement_items_of(Some(
                &std::fs::read_to_string(dir.path().join(AGREEMENTS_PATH)).unwrap(),
            ))[0]
                .id
                .clone();
            batch.batch_id = "22222222-2222-4222-8222-222222222222".into();
            batch.agreements = Some(vec![AgreementExtractionRecord::Reaffirm {
                id,
                date: date.into(),
                text: None,
                source: None,
            }]);
            // Use the old count-incrementing post image even for the same-day duplicate.
            let mut future = batch.clone();
            future.agreements = Some(vec![AgreementExtractionRecord::Reaffirm {
                id: agreement_items_of(Some(
                    &std::fs::read_to_string(dir.path().join(AGREEMENTS_PATH)).unwrap(),
                ))[0]
                    .id
                    .clone(),
                date: "2026-10-02".into(),
                text: None,
                source: None,
            }]);
            let mut recovery =
                plan_facts_mutation(&repo, &future, None, None, &Default::default(), None).unwrap();
            recovery.records_hash =
                facts_records_hash(&batch.records, batch.agreements.as_deref().unwrap());
            recovery.agreement_event_version = None;
            let path = &recovery.paths[0];
            if partial {
                repo.path_state
                    .set_index(&path.path, &path.post.index)
                    .unwrap();
                let crate::git::path_state::GitWorktreeIdentity::File(worktree) =
                    &path.post.worktree
                else {
                    panic!("published agreement path must have a worktree file");
                };
                repo.path_state
                    .write_worktree(&path.path, worktree)
                    .unwrap();
            }
            let pre = path.pre.clone();
            let mut published = None;
            let mut publisher = |next: &FactsApplyRecovery| {
                assert_eq!(repo.path_state.capture(AGREEMENTS_PATH).unwrap(), pre);
                published = Some(next.clone());
                Ok(())
            };
            let result = apply_facts_recovery_with_input(
                &repo,
                &recovery,
                1,
                &author,
                Some(AgreementRecoveryInput {
                    batch: &batch,
                    agreements_policy: None,
                    publish_recovery: Some(&mut publisher),
                }),
            )
            .unwrap();
            assert_eq!(result.outcome(), "committed");
            assert_eq!(published.unwrap().agreement_event_version, Some(1));
            let written = std::fs::read_to_string(dir.path().join(AGREEMENTS_PATH)).unwrap();
            assert_eq!(agreement_items_of(Some(&written))[0].n, expected);
            batch.batch_id = "33333333-3333-4333-8333-333333333333".into();
            assert_eq!(
                apply_facts_batch(&repo, batch, &author, ApplyFactsBatchOptions::default())
                    .unwrap()
                    .outcome(),
                "no_facts"
            );
            assert!(repo.status(&[] as &[&str]).unwrap().trim().is_empty());
        }
    }
}

#[test]
fn legacy_ledger_requires_original_batch_and_publisher() {
    let dir = tempfile::tempdir().unwrap();
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    let batch = FactsBatch {
        batch_id: "11111111-1111-4111-8111-111111111111".into(),
        records: vec![],
        agreements: Some(vec![AgreementExtractionRecord::New {
            applies: AgreementScope::Global,
            text: "Run tests".into(),
            date: "2026-10-01".into(),
            source: None,
        }]),
    };
    let mut recovery =
        plan_facts_mutation(&repo, &batch, None, None, &Default::default(), None).unwrap();
    recovery.agreement_event_version = None;
    assert!(upgrade_agreement_recovery(&repo, &recovery, None).is_err());
    let author = GitCommitAuthor {
        agent_id: "fixture".into(),
        author_name: "Fixture".into(),
        author_email: None,
    };
    assert!(apply_facts_recovery(&repo, &recovery, 1, &author).is_err());
}

struct RejectCommit {
    reject: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl crate::git::GitExec for RejectCommit {
    fn run(
        &self,
        argv: &[String],
        options: &crate::git::GitExecOptions,
    ) -> std::io::Result<crate::git::GitExecResult> {
        if self.reject.load(std::sync::atomic::Ordering::SeqCst)
            && argv.iter().any(|arg| arg == "commit")
        {
            return Ok(crate::git::GitExecResult {
                code: 1,
                stdout: String::new(),
                stderr: "fixture commit failure".into(),
            });
        }
        crate::git::system_git_exec().run(argv, options)
    }
}
#[test]
fn failed_commit_after_publication_leaves_resumable_upgraded_envelope() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let dir = tempfile::tempdir().unwrap();
    let reject = Arc::new(AtomicBool::new(false));
    let repo = crate::git::GitMemoryRepo::new(crate::git::GitMemoryRepoOptions {
        exec: Some(Arc::new(RejectCommit {
            reject: reject.clone(),
        })),
        ..crate::git::GitMemoryRepoOptions::new(dir.path(), "fixture")
    })
    .unwrap();
    repo.init(crate::git::InitializeGitRepoOptions {
        seed_files: vec![crate::git::GitSeedFile {
            relative_path: "seed.md".into(),
            content: "seed".into(),
        }],
        ..Default::default()
    })
    .unwrap();
    let batch = FactsBatch {
        batch_id: "11111111-1111-4111-8111-111111111111".into(),
        records: vec![],
        agreements: Some(vec![AgreementExtractionRecord::New {
            applies: AgreementScope::Global,
            text: "Run tests".into(),
            date: "2026-10-01".into(),
            source: None,
        }]),
    };
    let mut recovery =
        plan_facts_mutation(&repo, &batch, None, None, &Default::default(), None).unwrap();
    recovery.agreement_event_version = None;
    let author = GitCommitAuthor {
        agent_id: "fixture".into(),
        author_name: "Fixture".into(),
        author_email: None,
    };
    reject.store(true, Ordering::SeqCst);
    let mut published = None;
    let mut publisher = |next: &FactsApplyRecovery| {
        published = Some(next.clone());
        Ok(())
    };
    assert!(
        apply_facts_recovery_with_input(
            &repo,
            &recovery,
            1,
            &author,
            Some(AgreementRecoveryInput {
                batch: &batch,
                agreements_policy: None,
                publish_recovery: Some(&mut publisher)
            })
        )
        .is_err()
    );
    reject.store(false, Ordering::SeqCst);
    let published = published.unwrap();
    assert_eq!(
        apply_facts_recovery(&repo, &published, 1, &author)
            .unwrap()
            .outcome(),
        "committed"
    );
}
