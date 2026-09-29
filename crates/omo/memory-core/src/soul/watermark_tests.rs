use pretty_assertions::assert_eq;

use std::collections::BTreeMap;
use std::path::PathBuf;

use tempfile::TempDir;

use super::{ConsumeSoulNoticeOptions, SOUL_NOTICE_WATERMARK_FILENAME, consume_soul_notice_delta};
use crate::git::{GitCommitAuthor, GitMemoryRepo, GitMemoryRepoOptions, InitializeGitRepoOptions};

struct TestFixture {
    _root: TempDir,
    repo: GitMemoryRepo,
    notices_dir: PathBuf,
    locks_dir: PathBuf,
}

fn setup_fixture() -> TestFixture {
    let root = TempDir::new().expect("temp dir");
    let repo_dir = root.path().join("repo");
    let notices_dir = root.path().join("runtime").join("notices");
    let locks_dir = root.path().join("runtime").join("locks");

    std::fs::create_dir_all(&notices_dir).expect("create notices dir");
    std::fs::create_dir_all(&locks_dir).expect("create locks dir");

    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: repo_dir,
        agent_id: "agent-soul-test".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    repo.init(InitializeGitRepoOptions {
        seed_files: Vec::new(),
        author_name: Some("Soul Test Agent".to_string()),
        install_hooks: None,
    })
    .expect("init repo");

    TestFixture {
        _root: root,
        repo,
        notices_dir,
        locks_dir,
    }
}

fn commit_file(
    repo: &GitMemoryRepo,
    rel_path: &str,
    content: &str,
    message: &str,
    trailers: Option<BTreeMap<String, String>>,
) -> String {
    let full = repo.dir.join(rel_path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(&full, content).expect("write file");

    let mut full_message = message.to_string();
    if let Some(t) = trailers {
        full_message.push_str("\n\n");
        for (k, v) in t {
            full_message.push_str(&format!("{k}: {v}\n"));
        }
    }

    let author = GitCommitAuthor {
        agent_id: repo.agent_id.clone(),
        author_name: "Soul Test Agent".to_string(),
        author_email: None,
    };
    repo.commit_write(&[rel_path], &full_message, &author)
        .expect("commit write");
    repo.head().expect("head").expect("head exists")
}

#[test]
fn given_no_watermark_file_when_consumed_then_watermark_is_established_silently_at_head_with_no_notice()
 {
    let fixture = setup_fixture();
    let head = commit_file(
        &fixture.repo,
        "system/persona.md",
        "seed persona\n",
        "seed persona",
        None,
    );

    let options = ConsumeSoulNoticeOptions {
        notices_dir: fixture.notices_dir.clone(),
        locks_dir: fixture.locks_dir.clone(),
        wait_timeout_ms: Some(2_000),
    };

    let notice = consume_soul_notice_delta(&fixture.repo, &options).expect("consume");
    assert_eq!(notice, None);

    let watermark_path = fixture.notices_dir.join(SOUL_NOTICE_WATERMARK_FILENAME);
    let raw = std::fs::read_to_string(&watermark_path).expect("read watermark");
    let val: serde_json::Value = serde_json::from_str(&raw).expect("parse json");
    assert_eq!(val["version"], 1);
    assert_eq!(val["lastNotifiedHead"], head);
}

#[test]
fn given_watermark_behind_head_with_out_of_band_persona_commit_when_consumed_then_notice_emitted_and_watermark_advances()
 {
    let fixture = setup_fixture();
    commit_file(
        &fixture.repo,
        "system/persona.md",
        "seed persona\n",
        "seed persona",
        None,
    );

    let options = ConsumeSoulNoticeOptions {
        notices_dir: fixture.notices_dir.clone(),
        locks_dir: fixture.locks_dir.clone(),
        wait_timeout_ms: Some(2_000),
    };

    let first = consume_soul_notice_delta(&fixture.repo, &options).expect("first consume");
    assert_eq!(first, None);

    let mut trailers = BTreeMap::new();
    trailers.insert("Omo-Writer".to_string(), "reflection".to_string());
    let soul_sha = commit_file(
        &fixture.repo,
        "system/persona.md",
        "reflected persona\n",
        "chore(reflection): merge run abc",
        Some(trailers),
    );

    let notice = consume_soul_notice_delta(&fixture.repo, &options)
        .expect("second consume")
        .expect("notice present");
    assert_eq!(notice.sha, soul_sha);
    assert_eq!(notice.subject, "chore(reflection): merge run abc");

    let replay = consume_soul_notice_delta(&fixture.repo, &options).expect("replay consume");
    assert_eq!(replay, None);

    let watermark_path = fixture.notices_dir.join(SOUL_NOTICE_WATERMARK_FILENAME);
    let raw = std::fs::read_to_string(&watermark_path).expect("read watermark");
    let val: serde_json::Value = serde_json::from_str(&raw).expect("parse json");
    assert_eq!(val["lastNotifiedHead"], soul_sha);
}

#[test]
fn given_only_in_band_memory_tool_soul_commits_since_watermark_when_consumed_then_no_notice_is_emitted()
 {
    let fixture = setup_fixture();
    commit_file(
        &fixture.repo,
        "system/persona.md",
        "seed persona\n",
        "seed persona",
        None,
    );

    let options = ConsumeSoulNoticeOptions {
        notices_dir: fixture.notices_dir.clone(),
        locks_dir: fixture.locks_dir.clone(),
        wait_timeout_ms: Some(2_000),
    };

    consume_soul_notice_delta(&fixture.repo, &options).expect("establish");

    let mut in_band_trailers = BTreeMap::new();
    in_band_trailers.insert("Omo-Writer".to_string(), "memory-tool".to_string());
    commit_file(
        &fixture.repo,
        "system/persona.md",
        "edited in band\n",
        "chore: in band edit",
        Some(in_band_trailers),
    );

    let notice = consume_soul_notice_delta(&fixture.repo, &options).expect("consume");
    assert_eq!(notice, None);
}

fn options_for(fixture: &TestFixture) -> ConsumeSoulNoticeOptions {
    ConsumeSoulNoticeOptions {
        notices_dir: fixture.notices_dir.clone(),
        locks_dir: fixture.locks_dir.clone(),
        wait_timeout_ms: Some(2_000),
    }
}

fn read_watermark(fixture: &TestFixture) -> serde_json::Value {
    let raw = std::fs::read_to_string(fixture.notices_dir.join(SOUL_NOTICE_WATERMARK_FILENAME))
        .expect("read watermark");
    serde_json::from_str(&raw).expect("parse watermark")
}

fn writer(value: &str) -> Option<BTreeMap<String, String>> {
    Some(BTreeMap::from([(
        "Omo-Writer".to_string(),
        value.to_string(),
    )]))
}

#[test]
fn given_only_in_band_soul_commits_when_consumed_then_the_watermark_stays_put() {
    let fixture = setup_fixture();
    commit_file(
        &fixture.repo,
        "system/persona.md",
        "seed persona\n",
        "seed persona",
        None,
    );
    let options = options_for(&fixture);
    consume_soul_notice_delta(&fixture.repo, &options).expect("establish");
    let established = read_watermark(&fixture);
    let in_band = BTreeMap::from([
        ("Omo-Writer".to_string(), "memory-tool".to_string()),
        ("Omo-Session".to_string(), "session-1".to_string()),
        ("Omo-Turn".to_string(), "3".to_string()),
    ]);
    commit_file(
        &fixture.repo,
        "system/persona.md",
        "in-band edit\n",
        "edit my soul",
        Some(in_band),
    );

    let notice = consume_soul_notice_delta(&fixture.repo, &options).expect("consume");

    assert_eq!(notice, None);
    assert_eq!(read_watermark(&fixture), established);
}

#[test]
fn given_out_of_band_commit_touching_only_non_soul_paths_when_consumed_then_no_notice_is_emitted() {
    let fixture = setup_fixture();
    commit_file(
        &fixture.repo,
        "system/persona.md",
        "seed persona\n",
        "seed persona",
        None,
    );
    let options = options_for(&fixture);
    consume_soul_notice_delta(&fixture.repo, &options).expect("establish");
    commit_file(
        &fixture.repo,
        "notes/facts/2026-08.md",
        "- fact\n",
        "chore(facts): apply batch",
        writer("facts-extractor"),
    );

    let notice = consume_soul_notice_delta(&fixture.repo, &options).expect("consume");

    assert_eq!(notice, None);
}

#[test]
fn given_out_of_band_identity_commit_mixed_with_in_band_persona_commit_when_consumed_then_notice_names_out_of_band_commit()
 {
    let fixture = setup_fixture();
    commit_file(
        &fixture.repo,
        "system/persona.md",
        "seed persona\n",
        "seed persona",
        None,
    );
    let options = options_for(&fixture);
    consume_soul_notice_delta(&fixture.repo, &options).expect("establish");
    commit_file(
        &fixture.repo,
        "system/persona.md",
        "in-band edit\n",
        "edit my soul",
        writer("memory-tool"),
    );
    let dream_sha = commit_file(
        &fixture.repo,
        "system/identity.md",
        "- Name: Ada\n",
        "chore(dream): consolidate",
        writer("dream"),
    );

    let notice = consume_soul_notice_delta(&fixture.repo, &options)
        .expect("consume")
        .expect("notice present");

    assert_eq!(notice.sha, dream_sha);
    assert_eq!(notice.subject, "chore(dream): consolidate");
}

#[test]
fn given_corrupt_watermark_file_when_consumed_then_it_reestablishes_silently_at_head() {
    let fixture = setup_fixture();
    let head = commit_file(
        &fixture.repo,
        "system/persona.md",
        "seed persona\n",
        "seed persona",
        None,
    );
    std::fs::write(
        fixture.notices_dir.join(SOUL_NOTICE_WATERMARK_FILENAME),
        "not json at all",
    )
    .expect("write corrupt watermark");

    let notice = consume_soul_notice_delta(&fixture.repo, &options_for(&fixture)).expect("consume");

    assert_eq!(notice, None);
    assert_eq!(
        read_watermark(&fixture),
        serde_json::json!({"version": 1, "lastNotifiedHead": head})
    );
}

#[test]
fn given_watermark_naming_sha_missing_from_history_when_consumed_then_it_reestablishes_silently_at_head()
 {
    let fixture = setup_fixture();
    let head = commit_file(
        &fixture.repo,
        "system/persona.md",
        "seed persona\n",
        "seed persona",
        None,
    );
    let stale = serde_json::json!({"version": 1, "lastNotifiedHead": "0".repeat(40)});
    std::fs::write(
        fixture.notices_dir.join(SOUL_NOTICE_WATERMARK_FILENAME),
        format!("{stale}\n"),
    )
    .expect("write stale watermark");

    let notice = consume_soul_notice_delta(&fixture.repo, &options_for(&fixture)).expect("consume");

    assert_eq!(notice, None);
    assert_eq!(
        read_watermark(&fixture),
        serde_json::json!({"version": 1, "lastNotifiedHead": head})
    );
}
