use pretty_assertions::assert_eq;
use std::fs;

use crate::git::GitMemoryRepo;
use crate::git::repo_types::GitCommitAuthor;

use crate::tools::memfs::parse_memory_file;
use crate::tools::memory::{MemoryToolParams, MemoryToolProvenanceInput, run_memory_tool};
use crate::tools::soul::SOUL_EDIT_RESULT_LINE;

fn test_author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "agent-memory-test".to_string(),
        author_name: "Memory Test Agent".to_string(),
        author_email: Some("memory-test@example.com".to_string()),
    }
}

fn fixture() -> (tempfile::TempDir, GitMemoryRepo) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let author = test_author();
    let repo = GitMemoryRepo::open(temp_dir.path(), &author.agent_id).expect("repo");
    repo.init(None).expect("init");
    (temp_dir, repo)
}

#[test]
fn test_creates_memory_block_and_commits_locally() {
    let (_temp, repo) = fixture();
    let author = test_author();

    let res = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "create profile".to_string(),
            file_path: Some("system/profile.md".to_string()),
            description: Some("User profile".to_string()),
            file_text: Some("Name: Ada".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("create");

    assert!(res.result.contains("Memory create committed locally"));
    assert!(res.commit.is_some());

    let content = fs::read_to_string(repo.dir.join("system/profile.md")).expect("read");
    let parsed = parse_memory_file(&content).expect("parse");
    assert_eq!(parsed.frontmatter.description, "User profile");
    assert_eq!(parsed.body, "Name: Ada");
}

#[test]
fn test_str_replace_updates_first_occurrence() {
    let (_temp, repo) = fixture();
    let author = test_author();

    run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "seed note".to_string(),
            file_path: Some("notes.md".to_string()),
            description: Some("Notes".to_string()),
            file_text: Some("apple banana apple".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("create");

    run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("str_replace".to_string()),
            reason: "replace fruit".to_string(),
            file_path: Some("notes.md".to_string()),
            old_string: Some("apple".to_string()),
            new_string: Some("orange".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("replace");

    let content = fs::read_to_string(repo.dir.join("notes.md")).expect("read");
    let parsed = parse_memory_file(&content).expect("parse");
    assert_eq!(parsed.body, "orange banana apple");
}

#[test]
fn test_insert_clamps_line_indices() {
    let (_temp, repo) = fixture();
    let author = test_author();

    run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "seed list".to_string(),
            file_path: Some("list.md".to_string()),
            description: Some("List".to_string()),
            file_text: Some("line 2\nline 3".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("create");

    run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("insert".to_string()),
            reason: "prepend line".to_string(),
            file_path: Some("list.md".to_string()),
            insert_line: Some(0),
            insert_text: Some("line 1".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("prepend");

    let content = fs::read_to_string(repo.dir.join("list.md")).expect("read");
    let parsed = parse_memory_file(&content).expect("parse");
    assert_eq!(parsed.body, "line 1\nline 2\nline 3");
}

#[test]
fn test_rename_and_delete() {
    let (_temp, repo) = fixture();
    let author = test_author();

    run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "create item".to_string(),
            file_path: Some("old.md".to_string()),
            description: Some("Old item".to_string()),
            file_text: Some("to be renamed".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("create");

    run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("rename".to_string()),
            reason: "rename item".to_string(),
            old_path: Some("old.md".to_string()),
            new_path: Some("new.md".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("rename");

    assert!(!repo.dir.join("old.md").exists());
    assert!(repo.dir.join("new.md").exists());

    run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("delete".to_string()),
            reason: "delete item".to_string(),
            file_path: Some("new.md".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("delete");

    assert!(!repo.dir.join("new.md").exists());
}

#[test]
fn test_commits_omo_trailers_when_provenance_is_present() {
    let (_temp, repo) = fixture();
    let author = test_author();

    let res = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "track fact".to_string(),
            file_path: Some("notes/trailer.md".to_string()),
            description: Some("Trailer".to_string()),
            file_text: Some("body".to_string()),
            provenance: Some(MemoryToolProvenanceInput {
                session_id: "session-1".to_string(),
                user_turns: 3,
            }),
            ..Default::default()
        },
        None,
    )
    .expect("create with provenance");

    let head = res.commit.expect("commit").sha;
    let commit = repo
        .log(None)
        .expect("log")
        .into_iter()
        .find(|commit| commit.sha == head)
        .expect("head commit");
    assert_eq!(
        commit.trailers.get("Omo-Writer").map(String::as_str),
        Some("memory-tool")
    );
    assert_eq!(
        commit.trailers.get("Omo-Session").map(String::as_str),
        Some("session-1")
    );
    assert_eq!(
        commit.trailers.get("Omo-Turn").map(String::as_str),
        Some("3")
    );
    assert_eq!(commit.subject, "track fact");
}

#[test]
fn test_commits_plain_reason_without_trailers_when_provenance_is_absent() {
    // Pin `memory.ts:257-258`: absent provenance yields a plain reason commit with no trailers and
    // no error; a standalone tool call keeps working.
    let (_temp, repo) = fixture();
    let author = test_author();

    let res = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "plain reason".to_string(),
            file_path: Some("notes/plain.md".to_string()),
            description: Some("Plain".to_string()),
            file_text: Some("body".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("create without provenance");

    let head = res.commit.expect("commit").sha;
    let commit = repo
        .log(None)
        .expect("log")
        .into_iter()
        .find(|commit| commit.sha == head)
        .expect("head commit");
    assert_eq!(commit.subject, "plain reason");
    assert_eq!(commit.body.trim(), "");
    assert!(!commit.trailers.contains_key("Omo-Writer"));
    assert!(!commit.trailers.contains_key("Omo-Session"));
    assert!(!commit.trailers.contains_key("Omo-Turn"));
}

#[test]
fn test_delete_directory_reports_each_tracked_file() {
    // Pin `remove`: a directory delete reports every tracked file under it, so the soul-edit line
    // fires for a persona file instead of being suppressed by a bare directory label.
    let (_temp, repo) = fixture();
    let author = test_author();

    for (path, text) in [("system/persona.md", "persona"), ("system/other.md", "other")] {
        run_memory_tool(
            &repo,
            &author,
            &MemoryToolParams {
                command: Some("create".to_string()),
                reason: format!("create {path}"),
                file_path: Some(path.to_string()),
                description: Some("Block".to_string()),
                file_text: Some(text.to_string()),
                ..Default::default()
            },
            None,
        )
        .expect("create block");
    }

    let res = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("delete".to_string()),
            reason: "delete system dir".to_string(),
            file_path: Some("system".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("delete directory");

    assert!(!repo.dir.join("system").exists());
    assert!(res.result.contains(SOUL_EDIT_RESULT_LINE));
}

#[test]
fn test_non_origin_remote_reports_harness_sync() {
    // Pin `hasConfiguredRemote` scans the git config for any `[remote "<name>"]` section, not
    // only `remote.origin.url`.
    let (_temp, repo) = fixture();
    let author = test_author();
    repo.config_set("remote.upstream.url", "https://example.com/upstream.git")
        .expect("config_set upstream remote");

    let res = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "remote create".to_string(),
            file_path: Some("remote.md".to_string()),
            description: Some("Remote".to_string()),
            file_text: Some("x".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("create");

    assert!(res.result.contains("harness will sync after the turn"));
}
