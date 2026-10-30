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
            .contains("str_replace made no effective changes")
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

fn seed_read_only(repo: &GitMemoryRepo, path: &str, read_only: &str) {
    let full = repo.dir.join(path);
    fs::create_dir_all(full.parent().expect("parent")).expect("parent dir");
    fs::write(
        &full,
        format!("---\ndescription: Seed\nread_only: {read_only}\n---\nbody"),
    )
    .expect("seed");
    repo.commit_write(&[path], &format!("seed {path}"), &test_author())
        .expect("seed commit");
}

fn params(command: &str, file_path: Option<&str>) -> MemoryToolParams {
    MemoryToolParams {
        command: Some(command.to_string()),
        reason: command.to_string(),
        file_path: file_path.map(str::to_string),
        ..Default::default()
    }
}

// memory-validation.test.ts: read_only descendants reject a recursive directory delete.
#[test]
fn test_read_only_descendants_reject_recursive_delete() {
    let (_temp, repo) = fixture();
    seed_read_only(&repo, "reference/history/locked.md", "true");
    let head = repo.head().expect("head");
    let err = run_memory_tool(
        &repo,
        &test_author(),
        &params("delete", Some("reference/history")),
        None,
    )
    .unwrap_err();
    assert!(err.message.starts_with("memory: "), "{}", err.message);
    assert!(
        err.message.contains("locked.md is read_only"),
        "{}",
        err.message
    );
    assert_eq!(repo.head().expect("head"), head);
}

// memory-validation.test.ts: every editable mutation command rejects read_only content.
#[test]
fn test_every_editable_command_rejects_read_only_content() {
    let (_temp, repo) = fixture();
    seed_read_only(&repo, "locked.md", "true");
    let cases = [
        MemoryToolParams {
            old_string: Some("body".into()),
            new_string: Some("new".into()),
            ..params("str_replace", Some("locked"))
        },
        MemoryToolParams {
            insert_line: Some(1),
            insert_text: Some("new".into()),
            ..params("insert", Some("locked"))
        },
        params("delete", Some("locked")),
        MemoryToolParams {
            old_path: Some("locked".into()),
            new_path: Some("moved".into()),
            ..params("rename", None)
        },
        MemoryToolParams {
            description: Some("New".into()),
            ..params("update_description", Some("locked"))
        },
    ];
    for case in cases {
        let err = run_memory_tool(&repo, &test_author(), &case, None).unwrap_err();
        assert!(
            err.message.starts_with("memory: locked") && err.message.contains("is read_only"),
            "{:?}: {}",
            case.command,
            err.message
        );
    }
}

// memory-validation.test.ts: byte-identical description reports no effective changes, HEAD unchanged.
#[test]
fn test_byte_identical_description_reports_no_effective_changes() {
    let (_temp, repo) = fixture();
    seed_read_only(&repo, "note.md", "false");
    let head = repo.head().expect("head");
    let err = run_memory_tool(
        &repo,
        &test_author(),
        &MemoryToolParams {
            description: Some("Seed".into()),
            ..params("update_description", Some("note"))
        },
        None,
    )
    .unwrap_err();
    assert!(
        err.message
            .contains("update_description made no effective changes"),
        "{}",
        err.message
    );
    assert_eq!(repo.head().expect("head"), head);
}

// memory-validation.test.ts: an untracked empty directory delete reports made no changes.
#[test]
fn test_untracked_empty_directory_delete_reports_no_changes() {
    let (_temp, repo) = fixture();
    fs::create_dir(repo.dir.join("empty")).expect("empty dir");
    let err = run_memory_tool(
        &repo,
        &test_author(),
        &params("delete", Some("empty")),
        None,
    )
    .unwrap_err();
    assert!(
        err.message.contains("delete made no changes"),
        "{}",
        err.message
    );
}

// memory-validation.test.ts: unrelated dirty files make cleanCheck fail first with the porcelain listing.
#[test]
fn test_unrelated_dirty_files_fail_clean_check_first() {
    let (_temp, repo) = fixture();
    let head = repo.head().expect("head");
    fs::write(repo.dir.join("dirty.md"), "dirty").expect("dirty");
    let err = run_memory_tool(
        &repo,
        &test_author(),
        &MemoryToolParams {
            description: Some("New".into()),
            ..params("create", Some("new"))
        },
        None,
    )
    .unwrap_err();
    assert!(
        err.message.contains("Memory repo has uncommitted changes"),
        "{}",
        err.message
    );
    assert!(err.message.contains("?? dirty.md"), "{}", err.message);
    assert_eq!(
        fs::read_to_string(repo.dir.join("dirty.md")).expect("dirty"),
        "dirty"
    );
    assert!(!repo.dir.join("new.md").exists());
    assert_eq!(repo.head().expect("head"), head);
}

// memory-validation.test.ts: editable frontmatter with read_only false keeps body and flag through update_description.
#[test]
fn test_read_only_false_stays_editable_and_preserves_body_and_flag() {
    let (_temp, repo) = fixture();
    let full = repo.dir.join("system/note.md");
    fs::create_dir_all(full.parent().expect("parent")).expect("parent dir");
    fs::write(
        &full,
        "---\ndescription: Seed\nread_only: false\n---\nkeep body",
    )
    .expect("seed");
    repo.commit_write(&["system/note.md"], "seed note", &test_author())
        .expect("seed commit");
    run_memory_tool(
        &repo,
        &test_author(),
        &MemoryToolParams {
            description: Some("New description".into()),
            ..params("update_description", Some("system/note"))
        },
        None,
    )
    .expect("update_description on read_only false");
    let parsed =
        crate::memfs::frontmatter::parse_memory_file(&fs::read_to_string(&full).expect("read"))
            .expect("parse");
    assert_eq!(parsed.frontmatter.description, "New description");
    assert_eq!(parsed.frontmatter.read_only.as_deref(), Some("false"));
    assert_eq!(parsed.body, "keep body");
}
