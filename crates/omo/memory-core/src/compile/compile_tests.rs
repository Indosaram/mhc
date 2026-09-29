use std::collections::BTreeMap;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use super::{CompileMemoryBlockOptions, compile_memory_block};
use crate::compile::compile_test_support::{
    CompiledBlockStructure, memory, parse_compiled_block, repo_with,
};
use crate::git::{
    GitExec, GitExecOptions, GitExecResult, GitMemoryRepo, GitMemoryRepoOptions, GitSeedFile,
    InitializeGitRepoOptions, system_git_exec,
};

fn compile(repo: &GitMemoryRepo, agent_id: &str) -> String {
    compile_memory_block(
        repo,
        &CompileMemoryBlockOptions {
            agent_id: agent_id.to_string(),
        },
    )
    .expect("compile memory block")
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn structure(
    sections: &[&str],
    projection_paths: &[&str],
    memory_open_tags: &[&str],
    agent_id: &str,
) -> CompiledBlockStructure {
    CompiledBlockStructure {
        sections: strings(sections),
        projection_paths: strings(projection_paths),
        memory_open_tags: strings(memory_open_tags),
        metadata: BTreeMap::from([("agentId".to_string(), agent_id.to_string())]),
    }
}

#[test]
fn given_nested_committed_memory_when_compiled_then_block_sections_projection_order_and_metadata_values_form_the_structural_contract()
 {
    // given
    let (_temp, repo) = repo_with(vec![
        (
            "system/persona.md",
            &memory("PERSONA_DESCRIPTION", "PERSONA_BODY\n"),
        ),
        (
            "system/facts.md",
            &memory("FACTS_DESCRIPTION", "FACTS_BODY\n"),
        ),
        (
            "system/human/prefs/coding.md",
            &memory("PREFS_DESCRIPTION", "PREFS_BODY\n"),
        ),
        (
            "reference/details.md",
            &memory("REFERENCE_DESCRIPTION", "EXTERNAL_BODY_SENTINEL\n"),
        ),
        ("archive/diagram.png", "BINARY_BODY_SENTINEL"),
        ("README.MD", "UPPERCASE_BODY_SENTINEL"),
        (
            "skills/deploy/SKILL.md",
            &memory("SKILL_DESCRIPTION", "SKILL_BODY_SENTINEL\n"),
        ),
    ])
    .expect("repo init");

    // when
    let block = compile(&repo, "agent-golden");

    // then
    assert_eq!(
        parse_compiled_block(&block).expect("parse"),
        structure(
            &["self", "memory", "memory_metadata"],
            &[
                "system/persona.md",
                "system/facts.md",
                "system/human/prefs/coding.md"
            ],
            &["facts", "human", "prefs", "coding", "external_projection"],
            "agent-golden",
        )
    );
    assert!(block.contains("PERSONA_BODY"));
    assert!(block.contains("FACTS_BODY"));
    assert!(block.contains("PREFS_BODY"));
    assert!(!block.contains("EXTERNAL_BODY_SENTINEL"));
    assert!(!block.contains("BINARY_BODY_SENTINEL"));
    assert!(!block.contains("SKILL_BODY_SENTINEL"));
}

#[test]
fn given_a_committed_persona_and_identity_when_compiled_then_both_projection_paths_share_the_self_section_and_metadata_remains_structured()
 {
    // given
    let (_temp, repo) = repo_with(vec![
        (
            "system/persona.md",
            &memory("PERSONA_DESCRIPTION", "PERSONA_BODY\n"),
        ),
        (
            "system/identity.md",
            &memory("IDENTITY_DESCRIPTION", "IDENTITY_BODY\n"),
        ),
        (
            "system/facts.md",
            &memory("FACTS_DESCRIPTION", "FACTS_BODY\n"),
        ),
    ])
    .expect("repo init");

    // when
    let block = compile(&repo, "persona-identity-agent");

    // then
    assert_eq!(
        parse_compiled_block(&block).expect("parse"),
        structure(
            &["self", "memory", "memory_metadata"],
            &["system/persona.md", "system/identity.md", "system/facts.md"],
            &["facts"],
            "persona-identity-agent",
        )
    );
}

#[test]
fn given_only_a_committed_identity_when_compiled_then_it_renders_under_self_without_a_persona_projection()
 {
    // given
    let (_temp, repo) = repo_with(vec![(
        "system/identity.md",
        &memory("IDENTITY_DESCRIPTION", "IDENTITY_BODY\n"),
    )])
    .expect("repo init");

    // when
    let parsed = parse_compiled_block(&compile(&repo, "identity-agent")).expect("parse");

    // then
    assert_eq!(parsed.sections, strings(&["self", "memory_metadata"]));
    assert_eq!(parsed.projection_paths, strings(&["system/identity.md"]));
}

#[test]
fn given_an_empty_committed_repository_when_compiled_then_only_structured_metadata_is_emitted() {
    // given
    let (_temp, repo) = repo_with(vec![]).expect("repo init");

    // when
    let block = compile(&repo, "empty-agent");

    // then
    assert_eq!(
        parse_compiled_block(&block).expect("parse"),
        structure(&["memory_metadata"], &[], &[], "empty-agent")
    );
}

#[test]
fn given_only_a_committed_persona_when_compiled_then_its_body_is_projected_without_its_description()
{
    // given
    let (_temp, repo) = repo_with(vec![(
        "system/persona.md",
        &memory("DESCRIPTION_SENTINEL", "PERSONA_BODY_SENTINEL\n"),
    )])
    .expect("repo init");

    // when
    let block = compile(&repo, "persona-agent");
    let parsed = parse_compiled_block(&block).expect("parse");

    // then
    assert_eq!(parsed.sections, strings(&["self", "memory_metadata"]));
    assert_eq!(parsed.projection_paths, strings(&["system/persona.md"]));
    assert!(block.contains("PERSONA_BODY_SENTINEL"));
    assert!(!block.contains("DESCRIPTION_SENTINEL"));
}

#[test]
fn given_a_stable_identity_and_head_when_compiled_repeatedly_then_metadata_is_identity_only_and_byte_identical()
 {
    // given
    let (_temp, repo) = repo_with(vec![]).expect("repo init");

    // when
    let first = compile(&repo, "stable-agent");
    let second = compile(&repo, "stable-agent");

    // then
    assert_eq!(second, first);
    assert_eq!(
        parse_compiled_block(&first).expect("parse").metadata,
        BTreeMap::from([("agentId".to_string(), "stable-agent".to_string())])
    );
    for volatile in [
        "CONVERSATION_ID",
        "previous messages",
        "user turns since your last memory save",
        "Soul updated by",
    ] {
        assert!(
            !first.contains(volatile),
            "unexpected volatile metadata {volatile}"
        );
    }
}

#[test]
fn given_a_dirty_persona_edit_when_compiled_then_only_the_committed_head_body_sentinel_appears() {
    // given
    let (_temp, repo) = repo_with(vec![(
        "system/persona.md",
        &memory("PERSONA", "COMMITTED_BODY_SENTINEL\n"),
    )])
    .expect("repo init");
    std::fs::write(
        repo.dir.join("system/persona.md"),
        memory("PERSONA", "DIRTY_BODY_SENTINEL\n"),
    )
    .expect("dirty edit");

    // when
    let block = compile(&repo, "persona-agent");

    // then
    assert!(block.contains("COMMITTED_BODY_SENTINEL"));
    assert!(!block.contains("DIRTY_BODY_SENTINEL"));
}

/// Fails any `git show` of a `.png` blob so the test proves binaries are never read.
struct PngShowGuard {
    inner: Arc<dyn GitExec>,
}

impl GitExec for PngShowGuard {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        let reads_png = argv.first().is_some_and(|cmd| cmd == "show")
            && argv.iter().any(|arg| arg.ends_with(".png"));
        if reads_png {
            return Err(std::io::Error::other("binary blob was read"));
        }
        self.inner.run(argv, options)
    }
}

#[test]
fn given_a_committed_binary_projection_when_compiled_then_its_path_is_listed_without_reading_its_blob()
 {
    // given
    let temp = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: temp.path().to_path_buf(),
        agent_id: "fixture-agent".to_string(),
        exec: Some(Arc::new(PngShowGuard {
            inner: system_git_exec(),
        })),
        install_hooks: None,
    })
    .expect("open repo");
    repo.init(InitializeGitRepoOptions {
        seed_files: vec![GitSeedFile {
            relative_path: "assets/logo.png".to_string(),
            content: "BINARY_BODY_SENTINEL".to_string(),
        }],
        ..InitializeGitRepoOptions::default()
    })
    .expect("init repo");

    // when
    let block = compile(&repo, "binary-agent");
    let parsed = parse_compiled_block(&block).expect("parse");

    // then
    assert_eq!(parsed.sections, strings(&["memory", "memory_metadata"]));
    assert_eq!(parsed.memory_open_tags, strings(&["external_projection"]));
    assert!(block.contains("logo.png"));
    assert!(!block.contains("BINARY_BODY_SENTINEL"));
}

#[test]
fn given_an_unreadable_committed_system_file_when_compiled_then_its_structural_tag_is_skipped_best_effort()
 {
    // given
    let (_temp, repo) = repo_with(vec![
        (
            "system/persona.md",
            &memory("PERSONA", "READABLE_BODY_SENTINEL\n"),
        ),
        ("system/broken.md", "missing frontmatter"),
    ])
    .expect("repo init");

    // when
    let block = compile(&repo, "skip-agent");
    let parsed = parse_compiled_block(&block).expect("parse");

    // then
    assert_eq!(parsed.sections, strings(&["self", "memory_metadata"]));
    assert_eq!(parsed.projection_paths, strings(&["system/persona.md"]));
    assert!(block.contains("READABLE_BODY_SENTINEL"));
}
