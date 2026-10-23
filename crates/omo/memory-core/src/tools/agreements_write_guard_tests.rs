use tempfile::tempdir;

use super::refuse_agreements_violation;
use crate::agreements::types::AGREEMENTS_PATH;
use crate::git::GitCommitAuthor;

const HUMAN: &str = "- [global] Never push <!-- id: aaaaaaaaaa; by: human; n: 0; since: 2026-01-01; status: active -->";

fn file(body: &str) -> String {
    format!("---\ndescription: x\n---\n{body}\n")
}

#[test]
fn ignores_writes_that_do_not_touch_the_ledger() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    assert_eq!(
        refuse_agreements_violation(&repo, &["notes/a.md".to_string()]).expect("ok"),
        None
    );
}

#[test]
fn refuses_and_rolls_back_a_forged_human_item_on_a_new_file() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    std::fs::create_dir_all(dir.path().join("system")).expect("mkdir");
    std::fs::write(dir.path().join(AGREEMENTS_PATH), file(HUMAN)).expect("write");

    let violation =
        refuse_agreements_violation(&repo, &[AGREEMENTS_PATH.to_string()]).expect("guard");
    assert!(violation.is_some_and(|m| m.contains("which only /agreements add may write")));
    assert!(!dir.path().join(AGREEMENTS_PATH).exists());
    assert!(
        repo.status(&[] as &[&str])
            .expect("status")
            .trim()
            .is_empty()
    );
}

#[test]
fn refuses_and_restores_a_tracked_ledger_that_loses_a_human_item() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    std::fs::create_dir_all(dir.path().join("system")).expect("mkdir");
    std::fs::write(dir.path().join(AGREEMENTS_PATH), file(HUMAN)).expect("write");
    let author = GitCommitAuthor {
        agent_id: "a".to_string(),
        author_name: "A".to_string(),
        author_email: None,
    };
    repo.commit_write(&[AGREEMENTS_PATH], "human ledger", &author)
        .expect("commit");

    std::fs::write(dir.path().join(AGREEMENTS_PATH), file("")).expect("empty");
    let violation =
        refuse_agreements_violation(&repo, &[AGREEMENTS_PATH.to_string()]).expect("guard");
    assert!(violation.is_some_and(|m| m.contains("cannot be removed by automation")));
    assert_eq!(
        std::fs::read_to_string(dir.path().join(AGREEMENTS_PATH)).expect("read"),
        file(HUMAN)
    );
    assert!(
        repo.status(&[] as &[&str])
            .expect("status")
            .trim()
            .is_empty()
    );
}
