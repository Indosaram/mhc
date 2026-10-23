use pretty_assertions::assert_eq;
use tempfile::TempDir;

use super::{
    AGREEMENTS_NOTICE_WATERMARK_FILENAME, ConsumeAgreementsNoticeOptions,
    consume_agreements_notice_delta,
};
use crate::agreements::types::AGREEMENTS_PATH;
use crate::git::{GitCommitAuthor, GitMemoryRepo};

struct Fixture {
    _root: TempDir,
    repo: GitMemoryRepo,
    options: ConsumeAgreementsNoticeOptions,
}

fn fixture() -> Fixture {
    let root = TempDir::new().expect("tempdir");
    let repo_dir = root.path().join("repo");
    std::fs::create_dir_all(&repo_dir).expect("repo dir");
    let repo = crate::support::test_repo::init_test_repo(&repo_dir);
    let notices_dir = root.path().join("runtime").join("notices");
    let locks_dir = root.path().join("runtime").join("locks");
    std::fs::create_dir_all(&locks_dir).expect("locks dir");
    Fixture {
        options: ConsumeAgreementsNoticeOptions {
            notices_dir,
            locks_dir,
            wait_timeout_ms: Some(2_000),
        },
        repo,
        _root: root,
    }
}

fn commit_ledger(repo: &GitMemoryRepo, lines: &[&str]) {
    std::fs::create_dir_all(repo.dir.join("system")).expect("mkdir");
    let body = lines.join("\n");
    std::fs::write(
        repo.dir.join(AGREEMENTS_PATH),
        format!("---\ndescription: x\n---\n{body}\n"),
    )
    .expect("write");
    let author = GitCommitAuthor {
        agent_id: "a".to_string(),
        author_name: "A".to_string(),
        author_email: None,
    };
    repo.commit_write(&[AGREEMENTS_PATH], "ledger", &author)
        .expect("commit");
}

const FLAGGED: &str =
    "- [global] Run tests <!-- id: cccccccccc; by: agent; n: 3; status: needs-human -->";
const ACKED: &str = "- [global] Run tests <!-- id: cccccccccc; by: agent; n: 0; status: active -->";

fn announced(fixture: &Fixture) -> serde_json::Value {
    let raw = std::fs::read_to_string(
        fixture
            .options
            .notices_dir
            .join(AGREEMENTS_NOTICE_WATERMARK_FILENAME),
    )
    .expect("watermark");
    serde_json::from_str(&raw).expect("json")
}

#[test]
fn flagged_item_is_announced_once_and_reannounced_after_ack() {
    let fixture = fixture();
    assert!(
        consume_agreements_notice_delta(&fixture.repo, &fixture.options)
            .expect("none")
            .is_empty()
    );

    commit_ledger(&fixture.repo, &[FLAGGED]);
    let first = consume_agreements_notice_delta(&fixture.repo, &fixture.options).expect("first");
    assert_eq!(
        first
            .iter()
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>(),
        ["cccccccccc"]
    );
    assert_eq!(
        announced(&fixture)["announced"],
        serde_json::json!(["cccccccccc"])
    );
    assert!(
        consume_agreements_notice_delta(&fixture.repo, &fixture.options)
            .expect("second")
            .is_empty()
    );

    commit_ledger(&fixture.repo, &[ACKED]);
    assert!(
        consume_agreements_notice_delta(&fixture.repo, &fixture.options)
            .expect("acked")
            .is_empty()
    );
    assert_eq!(announced(&fixture)["announced"], serde_json::json!([]));

    commit_ledger(&fixture.repo, &[FLAGGED]);
    let again = consume_agreements_notice_delta(&fixture.repo, &fixture.options).expect("again");
    assert_eq!(again.len(), 1);
}

#[test]
fn corrupt_watermark_reads_as_empty() {
    let fixture = fixture();
    commit_ledger(&fixture.repo, &[FLAGGED]);
    std::fs::create_dir_all(&fixture.options.notices_dir).expect("notices");
    std::fs::write(
        fixture
            .options
            .notices_dir
            .join(AGREEMENTS_NOTICE_WATERMARK_FILENAME),
        "{not json",
    )
    .expect("corrupt");
    let fresh = consume_agreements_notice_delta(&fixture.repo, &fixture.options).expect("consume");
    assert_eq!(fresh.len(), 1);
}
