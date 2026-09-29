use std::collections::BTreeMap as HashMap;

use pretty_assertions::assert_eq;
use tempfile::tempdir;

use crate::git::path_state::{GitIndexIdentity, GitPathState, GitWorktreeIdentity};

use crate::facts::mutation_plan::{FactsApplyRecovery, FactsPostIdentity, FactsRecoveryPath};
use crate::facts::recovery_mutation::FactsMutationTransaction;

#[test]
fn test_transaction_fails_when_path_is_missing_from_initial_state() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    let initial = HashMap::new();
    let recovery = FactsApplyRecovery {
        version: 1,
        batch_id: "00000000-0000-4000-8000-000000000001".to_string(),
        head_before_apply: "head".to_string(),
        records_hash: "hash".to_string(),
        people: None,
        paths: vec![FactsRecoveryPath {
            path: "missing.md".to_string(),
            pre: GitPathState {
                index: None,
                worktree: GitWorktreeIdentity::Missing,
            },
            post: FactsPostIdentity {
                index: GitIndexIdentity {
                    mode: "100644".to_string(),
                    oid: "oid".to_string(),
                },
                worktree: GitWorktreeIdentity::File(
                    crate::git::path_state::GitWorktreeFileIdentity {
                        mode: 0o644,
                        oid: "oid".to_string(),
                    },
                ),
            },
        }],
    };

    let mut transaction = FactsMutationTransaction::new(&repo, &recovery, &initial);
    let result = transaction.restore_pre_state();

    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("Missing initial facts state: missing.md")
    );
}

#[test]
fn test_transaction_rollback_reverts_performed_changes() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    let path = "test_file.md".to_string();
    let initial_content = "initial content\n";
    let full_path = dir.path().join(&path);
    std::fs::write(&full_path, initial_content).expect("write file");

    let pre_state = repo.path_state.capture(&path).expect("capture initial");
    let mut initial = HashMap::new();
    initial.insert(path.clone(), pre_state.clone());

    let new_oid = repo
        .path_state
        .hash_worktree_blob("new content\n", true)
        .expect("hash blob");

    let recovery = FactsApplyRecovery {
        version: 1,
        batch_id: "00000000-0000-4000-8000-000000000002".to_string(),
        head_before_apply: "head".to_string(),
        records_hash: "hash".to_string(),
        people: None,
        paths: vec![FactsRecoveryPath {
            path: path.clone(),
            pre: pre_state,
            post: FactsPostIdentity {
                index: GitIndexIdentity {
                    mode: "100644".to_string(),
                    oid: new_oid.clone(),
                },
                worktree: GitWorktreeIdentity::File(
                    crate::git::path_state::GitWorktreeFileIdentity {
                        mode: 0o644,
                        oid: new_oid,
                    },
                ),
            },
        }],
    };

    let mut transaction = FactsMutationTransaction::new(&repo, &recovery, &initial);
    transaction.restore_pre_state().expect("restore pre state");
    transaction.rollback().expect("rollback succeeds");

    let current = repo
        .path_state
        .capture(&path)
        .expect("capture after rollback");
    assert_eq!(current.worktree, initial.get(&path).unwrap().worktree);
}
