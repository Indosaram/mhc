use pretty_assertions::assert_eq;
use std::fs;

use crate::git::GitMemoryRepo;
use crate::git::repo_types::GitCommitAuthor;

use crate::tools::memfs::parse_memory_file;
use crate::tools::memory::{MemoryToolParams, run_memory_tool};

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
