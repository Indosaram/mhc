use crate::git::GitMemoryRepo;
use crate::git::repo_types::GitCommitAuthor;

use crate::tools::memory::{MemoryToolParams, run_memory_tool};
use crate::tools::memory_apply_patch::{MemoryApplyPatchParams, run_memory_apply_patch};
use crate::tools::soul::{MEMORY_SOUL_EDIT_RESULT_TOKEN, SOUL_EDIT_RESULT_LINE};

fn test_author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "agent-soul".to_string(),
        author_name: "Soul Agent".to_string(),
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
fn test_appends_soul_edit_result_line_on_persona_edit() {
    let (_temp, repo) = fixture();
    let author = test_author();

    let res = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "initialize persona".to_string(),
            file_path: Some("system/persona.md".to_string()),
            description: Some("Agent persona".to_string()),
            file_text: Some("Helpful and direct".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("create");

    assert!(res.result.contains(SOUL_EDIT_RESULT_LINE));
    assert!(res.result.contains(MEMORY_SOUL_EDIT_RESULT_TOKEN));
}

#[test]
fn test_appends_soul_edit_result_line_on_identity_edit() {
    let (_temp, repo) = fixture();
    let author = test_author();

    let res = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "initialize identity".to_string(),
            file_path: Some("system/identity.md".to_string()),
            description: Some("Agent identity".to_string()),
            file_text: Some("Name: Senpi".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("create");

    assert!(res.result.contains(SOUL_EDIT_RESULT_LINE));
}

#[test]
fn test_does_not_append_soul_edit_line_on_regular_file() {
    let (_temp, repo) = fixture();
    let author = test_author();

    let res = run_memory_tool(
        &repo,
        &author,
        &MemoryToolParams {
            command: Some("create".to_string()),
            reason: "regular note".to_string(),
            file_path: Some("system/regular.md".to_string()),
            description: Some("Regular note".to_string()),
            file_text: Some("Hello world".to_string()),
            ..Default::default()
        },
        None,
    )
    .expect("create");

    assert!(!res.result.contains(SOUL_EDIT_RESULT_LINE));
}

#[test]
fn test_appends_soul_edit_line_on_memory_apply_patch() {
    let (_temp, repo) = fixture();
    let author = test_author();

    let patch = "*** Begin Patch\n*** Add File: system/persona.md\n+---\n+description: Persona\n+---\n+Updated persona\n*** End Patch";
    let res = run_memory_apply_patch(
        &repo,
        &MemoryApplyPatchParams {
            reason: "patch persona".to_string(),
            input: patch.to_string(),
            author,
            provenance: None,
        },
        None,
    )
    .expect("apply");

    assert!(res.message.contains(SOUL_EDIT_RESULT_LINE));
}
