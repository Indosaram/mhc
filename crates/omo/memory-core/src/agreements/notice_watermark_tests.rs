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

fn block_locks_dir(fixture: &Fixture) {
    // A locks dir that is a regular file makes every lock acquisition fail.
    std::fs::remove_dir_all(&fixture.options.locks_dir).expect("remove locks dir");
    std::fs::write(&fixture.options.locks_dir, "not a directory").expect("block locks dir");
}

#[test]
fn an_unchanged_needs_human_set_is_answered_without_taking_the_notice_lock() {
    // Given
    let fixture = fixture();
    commit_ledger(&fixture.repo, &[FLAGGED]);
    let first = consume_agreements_notice_delta(&fixture.repo, &fixture.options).expect("first");
    assert_eq!(first.len(), 1);
    block_locks_dir(&fixture);
    // When
    let again = consume_agreements_notice_delta(&fixture.repo, &fixture.options);
    // Then
    assert!(again.expect("optimistic path").is_empty());

    // When: the set changed, so the locked path runs and reports the lock failure.
    commit_ledger(&fixture.repo, &[ACKED]);
    // Then
    assert!(consume_agreements_notice_delta(&fixture.repo, &fixture.options).is_err());
}

#[test]
fn an_absent_or_invalid_watermark_with_nothing_flagged_is_rewritten_as_valid_empty_state() {
    for initial in [None, Some("{"), Some("{\"version\":2}")] {
        // Given
        let fixture = fixture();
        commit_ledger(&fixture.repo, &[ACKED]);
        let target = fixture
            .options
            .notices_dir
            .join(AGREEMENTS_NOTICE_WATERMARK_FILENAME);
        if let Some(initial) = initial {
            std::fs::create_dir_all(&fixture.options.notices_dir).expect("notices");
            std::fs::write(&target, initial).expect("seed");
        }
        // When
        let fresh =
            consume_agreements_notice_delta(&fixture.repo, &fixture.options).expect("consume");
        // Then
        assert!(fresh.is_empty(), "{initial:?}");
        assert_eq!(
            announced(&fixture),
            serde_json::json!({ "version": 1, "announced": [] }),
            "{initial:?}"
        );
    }
}

#[test]
fn a_failed_publication_surfaces_its_error_and_leaves_no_temporary() {
    // Given: the target name is taken by a non-empty directory, so the rename fails.
    let root = TempDir::new().expect("tempdir");
    let notices_dir = root.path().join("notices");
    let target = notices_dir.join(AGREEMENTS_NOTICE_WATERMARK_FILENAME);
    std::fs::create_dir_all(target.join("occupied")).expect("occupy target");
    let warnings = std::cell::RefCell::new(Vec::<String>::new());
    // When
    let result =
        super::write_watermark(&notices_dir, &["cccccccccc".to_string()], &|message, _| {
            warnings.borrow_mut().push(message.to_string())
        });
    // Then
    assert!(matches!(result, Err(super::AgreementsNoticeError::Io(_))));
    let leftovers: Vec<String> = std::fs::read_dir(&notices_dir)
        .expect("list")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.contains(".tmp-"))
        .collect();
    assert_eq!(leftovers, Vec::<String>::new());
    assert_eq!(warnings.into_inner(), Vec::<String>::new());
}
