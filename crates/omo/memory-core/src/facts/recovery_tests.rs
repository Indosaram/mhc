use pretty_assertions::assert_eq;
use tempfile::tempdir;

use crate::git::GitCommitAuthor;
use crate::git::path_state::{GitIndexIdentity, GitWorktreeIdentity};

use crate::facts::extraction::{
    ApplyFactsBatchOptions, FactsBatch, FactsExtractionRecord, apply_facts_batch,
};
use crate::facts::mutation_plan::{FactsApplyRecovery, plan_facts_mutation};
use crate::facts::recovery::{apply_facts_recovery, find_facts_batch_receipt};

fn author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "facts-recovery".to_string(),
        author_name: "Facts Recovery".to_string(),
        author_email: None,
    }
}

fn sample_batch() -> FactsBatch {
    FactsBatch {
        batch_id: "11111111-1111-4111-8111-111111111111".to_string(),
        records: vec![FactsExtractionRecord::Project {
            text: "Uses Bun.".to_string(),
            date: "2026-08-10".to_string(),
        }],
    }
}

#[test]
fn test_find_facts_batch_receipt_matches_the_batch_trailer_and_ignores_unknown_batches() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    apply_facts_batch(
        &repo,
        sample_batch(),
        &author(),
        ApplyFactsBatchOptions::default(),
    )
    .expect("apply batch");

    let receipt = find_facts_batch_receipt(&repo, &sample_batch().batch_id)
        .expect("receipt lookup")
        .expect("receipt commit");
    assert_eq!(
        receipt.trailers.get("Omo-Facts-Batch").map(String::as_str),
        Some(sample_batch().batch_id.as_str())
    );
    assert!(
        find_facts_batch_receipt(&repo, "00000000-0000-4000-8000-000000000000")
            .expect("receipt lookup")
            .is_none()
    );
}

#[test]
fn test_rejects_tracked_worktree_drift_without_changing_index_or_worktree_bytes() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    apply_facts_batch(
        &repo,
        sample_batch(),
        &author(),
        ApplyFactsBatchOptions::default(),
    )
    .expect("initial apply");

    let next = FactsBatch {
        batch_id: "22222222-2222-4222-8222-222222222222".to_string(),
        records: vec![FactsExtractionRecord::Project {
            text: "Uses TypeScript.".to_string(),
            date: "2026-08-11".to_string(),
        }],
    };
    let recovery = plan_facts_mutation(&repo, &next, None, None).expect("plan mutation");
    let path = &recovery.paths[0].path;
    let full_path = dir.path().join(path);
    std::fs::write(&full_path, "foreign tracked bytes\n").expect("write foreign bytes");
    let before = repo.path_state.capture(path).expect("capture before");

    let result = apply_facts_recovery(&repo, &recovery, 1, &author()).expect("apply recovery");

    assert_eq!(result.outcome(), "parent_dirty");
    assert_eq!(
        repo.path_state.capture(path).expect("capture after"),
        before
    );
    assert_eq!(
        std::fs::read_to_string(&full_path).expect("read full path"),
        "foreign tracked bytes\n"
    );
}

#[test]
fn test_rejects_tracked_index_drift_without_rewriting_either_surface() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    apply_facts_batch(
        &repo,
        sample_batch(),
        &author(),
        ApplyFactsBatchOptions::default(),
    )
    .expect("initial apply");

    let next = FactsBatch {
        batch_id: "22222222-2222-4222-8222-222222222222".to_string(),
        records: vec![FactsExtractionRecord::Project {
            text: "Uses TypeScript.".to_string(),
            date: "2026-08-11".to_string(),
        }],
    };
    let recovery = plan_facts_mutation(&repo, &next, None, None).expect("plan mutation");
    let path = &recovery.paths[0].path;
    let foreign_oid = repo
        .path_state
        .hash_index_blob(path, "foreign staged bytes\n", true)
        .expect("hash index blob");
    repo.path_state
        .set_index(
            path,
            &GitIndexIdentity {
                mode: "100644".to_string(),
                oid: foreign_oid,
            },
        )
        .expect("set index");
    let before = repo.path_state.capture(path).expect("capture before");

    let result = apply_facts_recovery(&repo, &recovery, 1, &author()).expect("apply recovery");

    assert_eq!(result.outcome(), "parent_dirty");
    assert_eq!(
        repo.path_state.capture(path).expect("capture after"),
        before
    );
}

#[test]
fn test_rejects_foreign_creation_at_a_planned_new_path_without_deleting_it() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    let recovery = plan_facts_mutation(&repo, &sample_batch(), None, None).expect("plan mutation");
    let path = &recovery.paths[0].path;
    let full_path = dir.path().join(path);
    std::fs::create_dir_all(full_path.parent().unwrap()).expect("create parent dirs");
    std::fs::write(&full_path, "foreign created bytes\n").expect("write foreign created");
    let before = repo.path_state.capture(path).expect("capture before");

    let result = apply_facts_recovery(&repo, &recovery, 1, &author()).expect("apply recovery");

    assert_eq!(result.outcome(), "parent_dirty");
    assert_eq!(
        repo.path_state.capture(path).expect("capture after"),
        before
    );
}

#[test]
fn test_restores_owned_partial_pre_post_residue_and_commits_the_exact_recorded_post_state() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    let batch = FactsBatch {
        batch_id: "33333333-3333-4333-8333-333333333333".to_string(),
        records: vec![
            FactsExtractionRecord::Project {
                text: "August.".to_string(),
                date: "2026-08-10".to_string(),
            },
            FactsExtractionRecord::Project {
                text: "September.".to_string(),
                date: "2026-09-01".to_string(),
            },
        ],
    };
    let recovery = plan_facts_mutation(&repo, &batch, None, None).expect("plan mutation");
    let first = &recovery.paths[0];
    repo.path_state
        .set_index(&first.path, &first.post.index)
        .expect("set index");
    let GitWorktreeIdentity::File(post_file) = &first.post.worktree else {
        panic!("planned post worktree must be a file");
    };
    repo.path_state
        .write_worktree(&first.path, post_file)
        .expect("write worktree");

    let result = apply_facts_recovery(&repo, &recovery, 2, &author()).expect("apply recovery");

    assert_eq!(result.outcome(), "committed");
    for entry in &recovery.paths {
        let captured = repo.path_state.capture(&entry.path).expect("capture");
        assert_eq!(captured.index, Some(entry.post.index.clone()));
        assert_eq!(captured.worktree, entry.post.worktree);
    }
}

#[test]
fn test_publishes_a_complete_blob_backed_envelope_before_the_first_affected_path_mutation() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    let mut published = false;
    let result = apply_facts_batch(
        &repo,
        sample_batch(),
        &author(),
        ApplyFactsBatchOptions {
            people: None,
            on_alias_tie: None,
            publish_recovery: Some(Box::new(|recovery: &FactsApplyRecovery| {
                published = true;
                let paths: Vec<&str> = recovery.paths.iter().map(|e| e.path.as_str()).collect();
                let mut sorted_paths = paths.clone();
                sorted_paths.sort();
                assert_eq!(paths, sorted_paths);
                Ok(())
            })),
        },
    )
    .expect("apply batch");

    assert!(published);
    assert_eq!(result.outcome(), "committed");
}

// --- recovery.test.ts / recovery-setter-race.test.ts ports that need no JS method monkeypatching.

mod race_ports {
    use std::sync::{Arc, Mutex};

    use pretty_assertions::assert_eq;
    use tempfile::TempDir;

    use crate::facts::extraction::{
        ApplyFactsBatchOptions, FactsBatch, FactsExtractionRecord, apply_facts_batch,
    };
    use crate::facts::mutation_plan::plan_facts_mutation;
    use crate::facts::recovery::{apply_facts_recovery, find_facts_batch_receipt};
    use crate::git::{
        GitExec, GitExecOptions, GitExecResult, GitMemoryRepo, GitMemoryRepoOptions,
        GitWorktreeIdentity, InitializeGitRepoOptions, system_git_exec,
    };

    type Injection = Box<dyn FnOnce() + Send>;

    struct HashInjectingExec {
        base: Arc<dyn GitExec>,
        injection: Arc<Mutex<Option<Injection>>>,
    }

    impl GitExec for HashInjectingExec {
        fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
            let result = self.base.run(argv, options)?;
            if argv.first().map(String::as_str) == Some("hash-object")
                && argv.iter().any(|a| a == "--no-filters")
                && let Some(op) = self.injection.lock().expect("lock").take()
            {
                op();
            }
            Ok(result)
        }
    }

    fn fixture() -> (TempDir, GitMemoryRepo, Arc<Mutex<Option<Injection>>>) {
        let tmp = TempDir::new().expect("tempdir");
        let injection: Arc<Mutex<Option<Injection>>> = Arc::new(Mutex::new(None));
        let exec: Arc<dyn GitExec> = Arc::new(HashInjectingExec {
            base: system_git_exec(),
            injection: Arc::clone(&injection),
        });
        let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
            exec: Some(exec),
            ..GitMemoryRepoOptions::new(tmp.path(), "facts-recovery")
        })
        .expect("repo");
        repo.init(InitializeGitRepoOptions::default())
            .expect("init");
        (tmp, repo, injection)
    }

    fn two_month_batch(id: &str) -> FactsBatch {
        FactsBatch {
            batch_id: id.to_string(),
            records: vec![
                FactsExtractionRecord::Project {
                    text: "August.".to_string(),
                    date: "2026-08-10".to_string(),
                },
                FactsExtractionRecord::Project {
                    text: "September.".to_string(),
                    date: "2026-09-01".to_string(),
                },
            ],
        }
    }

    fn receipts(repo: &GitMemoryRepo, batch_id: &str) -> usize {
        usize::from(
            find_facts_batch_receipt(repo, batch_id)
                .expect("receipt lookup")
                .is_some(),
        )
    }

    #[test]
    fn preserves_every_path_when_one_member_of_a_multi_path_batch_is_foreign() {
        let (tmp, repo, _) = fixture();
        let recovery = plan_facts_mutation(
            &repo,
            &two_month_batch("11111111-1111-4111-8111-111111111111"),
            None,
            None,
        )
        .expect("plan");
        let foreign = tmp.path().join(&recovery.paths[1].path);
        std::fs::create_dir_all(foreign.parent().expect("parent")).expect("mkdir");
        std::fs::write(&foreign, "foreign\n").expect("write");
        let paths: Vec<String> = recovery.paths.iter().map(|e| e.path.clone()).collect();
        let before = repo.path_state.capture_all(&paths).expect("before");

        let result = apply_facts_recovery(&repo, &recovery, 2, &super::author()).expect("apply");

        assert_eq!(result.outcome(), "parent_dirty");
        assert_eq!(repo.path_state.capture_all(&paths).expect("after"), before);
    }

    #[test]
    fn preserves_foreign_bytes_injected_after_hashing_an_existing_path_for_deletion() {
        let (tmp, repo, injection) = fixture();
        apply_facts_batch(
            &repo,
            FactsBatch {
                batch_id: "11111111-1111-4111-8111-111111111111".to_string(),
                records: vec![FactsExtractionRecord::Project {
                    text: "Initial August.".to_string(),
                    date: "2026-08-01".to_string(),
                }],
            },
            &super::author(),
            ApplyFactsBatchOptions::default(),
        )
        .expect("seed");
        let path = "notes/facts/2026-08.md";
        let before = repo.path_state.capture(path).expect("capture");
        assert!(matches!(before.worktree, GitWorktreeIdentity::File(_)));
        let receipts_before = receipts(&repo, "11111111-1111-4111-8111-111111111111");
        let target = tmp.path().join(path);
        // The file is moved aside before hashing; the injection recreates the target path.
        *injection.lock().expect("lock") = Some(Box::new(move || {
            std::fs::write(&target, "foreign existing deletion\n").expect("inject")
        }));

        let removed = repo
            .path_state
            .write_worktree_if_identity(path, &before.worktree, &GitWorktreeIdentity::Missing)
            .expect("conditional delete");

        assert!(!removed);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(path)).expect("read"),
            "foreign existing deletion\n"
        );
        assert_eq!(
            repo.path_state.capture(path).expect("after").index,
            before.index
        );
        assert_eq!(
            receipts(&repo, "11111111-1111-4111-8111-111111111111"),
            receipts_before
        );
    }

    #[test]
    fn preserves_a_foreign_replacement_injected_after_hashing_an_existing_tracked_path() {
        let (tmp, repo, injection) = fixture();
        apply_facts_batch(
            &repo,
            FactsBatch {
                batch_id: "11111111-1111-4111-8111-111111111111".to_string(),
                records: vec![FactsExtractionRecord::Project {
                    text: "Initial August.".to_string(),
                    date: "2026-08-01".to_string(),
                }],
            },
            &super::author(),
            ApplyFactsBatchOptions::default(),
        )
        .expect("seed");
        let recovery = plan_facts_mutation(
            &repo,
            &two_month_batch("22222222-2222-4222-8222-222222222222"),
            None,
            None,
        )
        .expect("plan");
        let (august, september) = ("notes/facts/2026-08.md", "notes/facts/2026-09.md");
        let index_before = repo
            .path_state
            .capture_all(&[august.to_string(), september.to_string()])
            .expect("before");
        let target = tmp.path().join(august);
        // Capture first, then arm the injection so it fires on the mutation-time hash only.
        let observed = repo.path_state.capture(august).expect("capture");
        let expected = observed.worktree.clone();
        *injection.lock().expect("lock") = Some(Box::new(move || {
            std::fs::write(&target, "foreign existing replacement\n").expect("inject")
        }));
        let next = recovery
            .paths
            .iter()
            .find(|e| e.path == august)
            .expect("august")
            .post
            .worktree
            .clone();

        let written = repo
            .path_state
            .write_worktree_if_identity(august, &expected, &next)
            .expect("conditional write");

        assert!(!written);
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(august)).expect("read"),
            "foreign existing replacement\n"
        );
        assert_eq!(
            repo.path_state.capture(august).expect("a").index,
            index_before.get(august).and_then(|s| s.index.clone())
        );
        assert_eq!(
            repo.path_state.capture(september).expect("s"),
            recovery
                .paths
                .iter()
                .find(|e| e.path == september)
                .expect("sep")
                .pre
        );
        assert_eq!(receipts(&repo, "22222222-2222-4222-8222-222222222222"), 0);
    }
}
