use std::fs;

use crate::git::GitMemoryRepo;
use crate::git::repo_types::GitCommitAuthor;

use crate::tools::memory::{MemoryToolParams, run_memory_tool};

fn test_author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "agent-val".to_string(),
        author_name: "Validation Agent".to_string(),
        author_email: None,
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
fn test_validates_required_parameters() {
    let (_temp, repo) = fixture();
    let author = test_author();

    let err_reason = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: String::new(),
            ..Default::default()
        },
        None,
    )
    .unwrap_err();
    assert!(
        err_reason
            .message
            .contains("'reason' must be a non-empty string")
    );

    let err_cmd = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: None,
            reason: "valid reason".to_string(),
            ..Default::default()
        },
        None,
    )
    .unwrap_err();
    assert!(
        err_cmd
            .message
            .contains("'command' must be a non-empty string")
    );

    let err_field = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "valid reason".to_string(),
            file_path: None,
            description: Some("desc".to_string()),
            ..Default::default()
        },
        None,
    )
    .unwrap_err();
    assert!(
        err_field
            .message
            .contains("create: 'file_path' must be a non-empty string")
    );
}

#[test]
fn test_rejects_modifying_readonly_blocks() {
    let (_temp, repo) = fixture();
    let author = test_author();

    let readonly_content = "---\ndescription: Protected\nread_only: true\n---\nCannot touch";
    fs::write(repo.dir.join("protected.md"), readonly_content).expect("write");
    repo.commit_write(&["protected.md"], "seed readonly", &author)
        .expect("commit");

    let err = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("str_replace".to_string()),
            reason: "try change".to_string(),
            file_path: Some("protected.md".to_string()),
            old_string: Some("Cannot".to_string()),
            new_string: Some("Can".to_string()),
            ..Default::default()
        },
        None,
    )
    .unwrap_err();

    assert!(err.message.contains("is read_only and cannot be modified"));
}

#[test]
fn test_rejects_utf16_and_invalid_utf8() {
    let (_temp, repo) = fixture();
    let author = test_author();

    let utf16_bytes = vec![0xff, 0xfe, b'a', 0x00, b'\n', 0x00];
    fs::write(repo.dir.join("utf16.md"), utf16_bytes).expect("write");
    repo.commit_write(&["utf16.md"], "seed utf16", &author)
        .expect("commit");

    let err16 = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("str_replace".to_string()),
            reason: "try utf16".to_string(),
            file_path: Some("utf16.md".to_string()),
            old_string: Some("a".to_string()),
            new_string: Some("b".to_string()),
            ..Default::default()
        },
        None,
    )
    .unwrap_err();
    assert!(
        err16
            .message
            .contains("is UTF-16 encoded; convert it to UTF-8")
    );

    let bad_utf8 = vec![0x80, 0x81, 0x82];
    fs::write(repo.dir.join("bad.md"), bad_utf8).expect("write");
    repo.commit_write(&["bad.md"], "seed bad", &author)
        .expect("commit");

    let err_bad = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("str_replace".to_string()),
            reason: "try bad utf8".to_string(),
            file_path: Some("bad.md".to_string()),
            old_string: Some("a".to_string()),
            new_string: Some("b".to_string()),
            ..Default::default()
        },
        None,
    )
    .unwrap_err();
    assert!(
        err_bad
            .message
            .contains("is not valid UTF-8; convert it to UTF-8")
    );
}

#[test]
fn test_rejects_no_changes_and_missing_strings() {
    let (_temp, repo) = fixture();
    let author = test_author();

    run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "seed note".to_string(),
            file_path: Some("note.md".to_string()),
            description: Some("Note".to_string()),
            file_text: Some("apple".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("create");

    let err_missing = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("str_replace".to_string()),
            reason: "replace".to_string(),
            file_path: Some("note.md".to_string()),
            old_string: Some("missing".to_string()),
            new_string: Some("found".to_string()),
            ..Default::default()
        },
        None,
    )
    .unwrap_err();
    assert!(
        err_missing
            .message
            .contains("old_string was not found in the target memory block")
    );

    let err_no_change = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("str_replace".to_string()),
            reason: "replace same".to_string(),
            file_path: Some("note.md".to_string()),
            old_string: Some("apple".to_string()),
            new_string: Some("apple".to_string()),
            ..Default::default()
        },
        None,
    )
    .unwrap_err();
    assert!(
        err_no_change
            .message
            .contains("str_replace made no changes")
    );
}

#[test]
fn test_rejects_path_traversal() {
    let (_temp, repo) = fixture();
    let author = test_author();

    let err = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "traversal".to_string(),
            file_path: Some("../outside.md".to_string()),
            description: Some("Escape".to_string()),
            ..Default::default()
        },
        None,
    )
    .unwrap_err();

    assert!(
        err.message
            .contains("contains invalid path traversal segment")
            || err.message.contains("escapes")
    );
}
