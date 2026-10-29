use std::path::PathBuf;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use crate::git::exec::{GitExec, system_git_exec};
use crate::git::{
    GitCommitAuthor, GitMemoryRepo, GitMemoryRepoOptions, GitSeedFile, InitializeGitRepoOptions,
};
use crate::memfs::{
    MAX_DESCRIPTION_LENGTH, MemoryFrontmatter, describe_frontmatter_violation, install_hooks,
    parse_memory_file, render_memory_file, resolve_hooks_dir,
};

use crate::reflection::completion_validation::{CompletionValidation, validate_completion};
use crate::reflection::description_preflight::{
    TrimmedDescription, preflight_reflection_descriptions,
};
use crate::reflection::machine::ReflectionOutcome;
use crate::reflection::worktree::{
    ReflectionFinalizeMode, ReflectionWorktree, create_reflection_worktree,
    finalize_reflection_worktree,
};

const PATH: &str = "reference/note.md";
const BODY: &str = "\n# Durable note\r\n\r\nBody stays byte-identical.  \r\n";
const AUTHOR_NAME: &str = "Reflection Agent";
const AUTHOR_EMAIL: &str = "reflection@omo.local";

fn oversized() -> String {
    format!("{}ending", "summary ".repeat(139))
}

fn memory_file(description: &str, body: &str) -> String {
    render_memory_file(
        &MemoryFrontmatter {
            description: description.to_string(),
            read_only: None,
            kind: None,
            aliases: None,
            extra: None,
        },
        body,
    )
    .expect("render memory file")
}

struct Fixture {
    _temp: TempDir,
    parent: GitMemoryRepo,
    worktree: ReflectionWorktree,
    child: GitMemoryRepo,
    exec: Arc<dyn GitExec>,
    tip: String,
}

fn fixture(content: &str, existing: bool, base_content: Option<String>) -> Fixture {
    let base_content = base_content.unwrap_or_else(|| memory_file(&"a".repeat(937), BODY));
    let temp = TempDir::new().expect("temp dir");
    let mut seeds = vec![GitSeedFile {
        relative_path: "reference/base.md".to_string(),
        content: memory_file("Base", "base\n"),
    }];
    if existing {
        seeds.push(GitSeedFile {
            relative_path: PATH.to_string(),
            content: base_content,
        });
    }
    let parent = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: temp.path().join("memory"),
        agent_id: "agent-one".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");
    parent
        .init(InitializeGitRepoOptions {
            seed_files: seeds,
            author_name: Some(AUTHOR_NAME.to_string()),
            install_hooks: None,
        })
        .expect("repo init");
    let exec = system_git_exec();
    let worktree = create_reflection_worktree(
        &parent,
        "description",
        &temp.path().join("worktrees"),
        exec.as_ref(),
        None,
    )
    .expect("create worktree");
    let child = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: worktree.dir.clone(),
        agent_id: "agent-one".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open child repo");
    std::fs::write(worktree.dir.join(PATH), content).expect("write note");
    child
        .commit_write(
            &[PATH],
            "reflection expanded summary",
            &GitCommitAuthor {
                agent_id: "agent-one".to_string(),
                author_name: AUTHOR_NAME.to_string(),
                author_email: Some(AUTHOR_EMAIL.to_string()),
            },
        )
        .expect("commit note");
    let tip = child.head().expect("head").expect("reflection tip");
    Fixture {
        _temp: temp,
        parent,
        worktree,
        child,
        exec,
        tip,
    }
}

fn git_out(exec: &dyn GitExec, dir: &std::path::Path, args: &[&str]) -> String {
    let result = exec.run_in(dir, args).expect("git run");
    assert_eq!(result.code, 0, "git {}: {}", args.join(" "), result.stderr);
    result.stdout
}

fn read_note(worktree: &ReflectionWorktree) -> String {
    std::fs::read_to_string(worktree.dir.join(PATH)).expect("read note")
}

fn preflight(fixture: &Fixture) -> Vec<TrimmedDescription> {
    preflight_reflection_descriptions(
        &fixture.worktree,
        &fixture.worktree.base_commit_sha,
        fixture.exec.as_ref(),
        None,
    )
}

#[test]
fn given_a_noncanonical_header_when_trimmed_then_every_other_byte_stays_in_order() {
    let oversized = oversized();
    let before = format!(
        "---\r\nkind: note\r\nlimit: 5000\r\nprovenance: \"legacy receipt\"\r\ndescription: {oversized}\r\nname: 'x'\r\n---\r\n{BODY}"
    );
    let fixture = fixture(&before, true, None);
    let expected = format!("description: {}summary…", "summary ".repeat(127));

    let trimmed = preflight(&fixture);

    assert_eq!(trimmed.len(), 1);
    assert_eq!(
        read_note(&fixture.worktree),
        before.replace(&format!("description: {oversized}"), &expected)
    );
}

#[test]
fn given_a_body_only_edit_of_a_legacy_oversized_header_when_preflight_runs_then_nothing_changes() {
    let base_content = memory_file(&oversized(), BODY);
    let content = format!("{base_content}new body line\n");
    let fixture = fixture(&content, true, Some(base_content));
    assert!(matches!(
        validate_completion(
            &fixture.worktree,
            &fixture.worktree.base_commit_sha,
            fixture.exec.as_ref()
        ),
        CompletionValidation::Valid { .. }
    ));

    assert_eq!(preflight(&fixture), Vec::new());

    assert_eq!(fixture.child.head().expect("head"), Some(fixture.tip.clone()));
    assert_eq!(read_note(&fixture.worktree), content);
    assert!(matches!(
        validate_completion(
            &fixture.worktree,
            &fixture.worktree.base_commit_sha,
            fixture.exec.as_ref()
        ),
        CompletionValidation::Valid { .. }
    ));
}

#[test]
fn given_a_note_with_an_oversized_summary_when_preflight_runs_then_one_hooked_commit_preserves_the_body() {
    for (existing, leading) in [(true, false), (false, false), (true, true)] {
        let oversized = oversized();
        let original = if leading {
            format!("   {oversized}")
        } else {
            oversized.clone()
        };
        let input = format!(
            "---\ndescription: {}\n---\n{BODY}",
            serde_json::to_string(&original).expect("json string")
        );
        let fixture = fixture(&input, existing, None);
        assert_eq!(oversized.chars().count(), 1118);
        assert_eq!(
            describe_frontmatter_violation(&input),
            Some(format!(
                "'description' exceeds 1024 characters ({})",
                original.chars().count()
            ))
        );
        install_hooks(&fixture.worktree.dir).expect("install hooks");

        let trimmed = preflight(&fixture);

        let content = read_note(&fixture.worktree);
        let parsed = parse_memory_file(&content).expect("parse trimmed note");
        let expected_description = if leading {
            format!("   {}summary…", "summary ".repeat(126))
        } else {
            format!("{}summary…", "summary ".repeat(127))
        };
        assert_eq!(parsed.frontmatter.description, expected_description);
        assert!(
            parsed.frontmatter.description.chars().count() <= MAX_DESCRIPTION_LENGTH,
            "{}",
            parsed.frontmatter.description.chars().count()
        );
        assert_eq!(parsed.body, BODY);
        assert_eq!(describe_frontmatter_violation(&content), None);
        assert_eq!(
            trimmed,
            vec![TrimmedDescription {
                path: PATH.to_string(),
                original_length: original.chars().count(),
                new_length: parsed.frontmatter.description.chars().count(),
            }]
        );
        let head = git_out(fixture.exec.as_ref(), &fixture.worktree.dir, &["rev-parse", "HEAD"])
            .trim()
            .to_string();
        let parent = git_out(
            fixture.exec.as_ref(),
            &fixture.worktree.dir,
            &["rev-parse", "HEAD^"],
        )
        .trim()
        .to_string();
        assert_eq!(parent, fixture.tip);
        let author = git_out(
            fixture.exec.as_ref(),
            &fixture.worktree.dir,
            &["log", "-1", "--format=%an%x00%ae"],
        );
        let mut parts = author.trim_end().split('\0');
        assert_eq!(parts.next(), Some(AUTHOR_NAME));
        assert_eq!(parts.next(), Some(AUTHOR_EMAIL));
        let message = git_out(
            fixture.exec.as_ref(),
            &fixture.worktree.dir,
            &["log", "-1", "--format=%B"],
        );
        assert!(
            message.contains(&format!(
                "Omo-Description-Trimmed: {PATH} ({} -> {})",
                original.chars().count(),
                parsed.frontmatter.description.chars().count()
            )),
            "{message}"
        );
        assert_eq!(
            fixture.parent.head().expect("head"),
            Some(fixture.worktree.base_commit_sha.clone())
        );
        assert!(matches!(
            validate_completion(
                &fixture.worktree,
                &fixture.worktree.base_commit_sha,
                fixture.exec.as_ref()
            ),
            CompletionValidation::Valid { .. }
        ));
        assert_eq!(preflight(&fixture), Vec::new());
        assert_eq!(
            git_out(fixture.exec.as_ref(), &fixture.worktree.dir, &["rev-parse", "HEAD"])
                .trim()
                .to_string(),
            head
        );
    }
}

#[test]
fn given_an_oversized_summary_and_a_duplicate_key_when_preflight_runs_then_the_note_is_untouched() {
    let content = format!(
        "---\ndescription: {}\nkind: note\nkind: duplicate\n---\n{BODY}",
        oversized()
    );
    let fixture = fixture(&content, true, None);

    assert_eq!(preflight(&fixture), Vec::new());

    assert_eq!(fixture.child.head().expect("head"), Some(fixture.tip.clone()));
    assert_eq!(read_note(&fixture.worktree), content);
    match validate_completion(
        &fixture.worktree,
        &fixture.worktree.base_commit_sha,
        fixture.exec.as_ref(),
    ) {
        CompletionValidation::NoChanges { dropped, .. } => {
            assert_eq!(
                dropped,
                vec![crate::reflection::completion_repair::DroppedEntry {
                    path: PATH.to_string(),
                    reason: "invalid frontmatter: duplicate frontmatter key 'kind'".to_string(),
                }]
            );
        }
        other => panic!("expected NoChanges completion, got {other:?}"),
    }
}

#[test]
fn given_no_oversized_summary_when_preflight_runs_then_it_makes_no_commit() {
    let content = memory_file("Short summary", BODY);
    let fixture = fixture(&content, true, None);

    assert_eq!(preflight(&fixture), Vec::new());

    assert_eq!(fixture.child.head().expect("head"), Some(fixture.tip.clone()));
    assert_eq!(read_note(&fixture.worktree), content);
}

#[test]
fn given_an_oversized_word_when_preflight_runs_then_it_hard_cuts_within_the_limit() {
    let fixture = fixture(&memory_file(&"x".repeat(1118), BODY), true, None);

    let trimmed = preflight(&fixture);

    assert_eq!(trimmed.len(), 1);
    let parsed = parse_memory_file(&read_note(&fixture.worktree)).expect("parse note");
    assert_eq!(
        parsed.frontmatter.description,
        format!("{}…", "x".repeat(1023))
    );
}

#[test]
fn given_an_emoji_across_the_hard_cut_boundary_when_preflight_runs_then_only_complete_code_points_remain() {
    let description = format!("{}😀{}", "x".repeat(1022), "x".repeat(94));
    let fixture = fixture(&memory_file(&description, BODY), true, None);

    let trimmed = preflight(&fixture);

    assert_eq!(trimmed.len(), 1);
    let parsed = parse_memory_file(&read_note(&fixture.worktree)).expect("parse note");
    assert_eq!(
        parsed.frontmatter.description,
        format!("{}…", "x".repeat(1022))
    );
    assert!(!parsed.frontmatter.description.contains('😀'));
}

#[test]
fn given_a_refusing_commit_hook_when_preflight_runs_then_it_restores_only_its_edits() {
    let input = memory_file(&oversized(), BODY);
    let fixture = fixture(&input, true, None);
    install_hooks(&fixture.worktree.dir).expect("install hooks");
    std::fs::write(
        resolve_hooks_dir(&fixture.worktree.dir).join("pre-commit"),
        "#!/bin/sh\nexit 1\n",
    )
    .expect("refusing hook");

    assert_eq!(preflight(&fixture), Vec::new());

    assert_eq!(fixture.child.head().expect("head"), Some(fixture.tip.clone()));
    assert_eq!(
        git_out(fixture.exec.as_ref(), &fixture.worktree.dir, &["status", "--porcelain"]),
        ""
    );
    assert_eq!(read_note(&fixture.worktree), input);
    match validate_completion(
        &fixture.worktree,
        &fixture.worktree.base_commit_sha,
        fixture.exec.as_ref(),
    ) {
        CompletionValidation::NoChanges { dropped, .. } => {
            assert_eq!(
                dropped,
                vec![crate::reflection::completion_repair::DroppedEntry {
                    path: PATH.to_string(),
                    reason:
                        "invalid frontmatter: 'description' exceeds 1024 characters (1118)"
                            .to_string(),
                }]
            );
        }
        other => panic!("expected NoChanges completion, got {other:?}"),
    }
}

#[test]
fn given_an_existing_note_expanded_to_1118_characters_when_finalized_then_preflight_repairs_it() {
    let fixture = fixture(&memory_file(&oversized(), BODY), true, None);
    install_hooks(&fixture.worktree.dir).expect("install hooks");

    let result = finalize_reflection_worktree(
        &fixture.worktree,
        ReflectionFinalizeMode::Auto {
            summary: "shorten reflection description".to_string(),
            run_id: None,
            allowed_paths: None,
        },
        |operation| operation(),
    );

    assert_eq!(result.status, ReflectionOutcome::Merged);
    assert!(result.cleanup.worktree_removed);
    assert!(result.cleanup.branch_removed);
    let merged: PathBuf = fixture.parent.dir.join(PATH);
    let parsed = parse_memory_file(&std::fs::read_to_string(&merged).expect("read merged note"))
        .expect("parse merged note");
    assert_eq!(
        parsed.frontmatter.description,
        format!("{}summary…", "summary ".repeat(127))
    );
    assert_eq!(parsed.body, BODY);
}
