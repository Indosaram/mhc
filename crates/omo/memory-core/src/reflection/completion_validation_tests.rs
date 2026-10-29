use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;

use tempfile::TempDir;

use crate::git::exec::{GitExec, GitExecBytesResult, GitExecOptions, GitExecResult, system_git_exec};
use crate::git::{
    GitCommitAuthor, GitMemoryRepo, GitMemoryRepoOptions, GitSeedFile, InitializeGitRepoOptions,
};

use crate::reflection::completion_repair::describe_dropped_entries;
use crate::reflection::completion_validation::{
    CompletionValidation, DreamTokenBudgetValidation, validate_completion, validate_dream_token_budget,
};
use crate::reflection::machine::DreamOrigin;
use crate::reflection::worktree::{ReflectionWorktree, create_reflection_worktree};

const PEM: &str = "-----BEGIN RSA PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSk\n-----END RSA PRIVATE KEY-----\n";
const GOOD: &str = "---\ndescription: A good note\n---\n\nbody\n";
const LEGACY: &str = "---\ndescription: Run the chain by default: verify, merge\n---\n\nlegacy body\n";

#[test]
fn pressure_dream_budget_rejects_committed_token_count_at_target() {
    assert!(matches!(validate_dream_token_budget(DreamOrigin::Pressure, 100, 100), DreamTokenBudgetValidation::BudgetNotMet { .. }));
    assert!(matches!(validate_dream_token_budget(DreamOrigin::Idle, 1, 100), DreamTokenBudgetValidation::Valid { .. }));
}

struct Fixture {
    _temp: TempDir,
    _repo: GitMemoryRepo,
    worktree: ReflectionWorktree,
    exec: Arc<dyn GitExec>,
}

fn fixture(seeds: &[(&str, &str)], run_id: &str) -> Fixture {
    let temp = TempDir::new().expect("temp dir");
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: temp.path().join("memory"),
        agent_id: "agent-one".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");
    repo.init(InitializeGitRepoOptions {
        seed_files: seeds
            .iter()
            .map(|(path, content)| GitSeedFile {
                relative_path: (*path).to_string(),
                content: (*content).to_string(),
            })
            .collect(),
        author_name: Some("Reflection Agent".to_string()),
        install_hooks: None,
    })
    .expect("repo init");
    let exec = system_git_exec();
    let worktree = create_reflection_worktree(
        &repo,
        run_id,
        &temp.path().join("worktrees"),
        exec.as_ref(),
        None,
    )
    .expect("create worktree");
    Fixture {
        _temp: temp,
        _repo: repo,
        worktree,
        exec,
    }
}

fn git_out(exec: &dyn GitExec, dir: &Path, args: &[&str]) -> String {
    let result = exec.run_in(dir, args).expect("git run");
    assert_eq!(result.code, 0, "git {}: {}", args.join(" "), result.stderr);
    result.stdout
}

fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(full, content).expect("write file");
    }
}

fn commit_files(exec: &dyn GitExec, dir: &Path, files: &[(&str, &str)], message: &str) {
    write_files(dir, files);
    git_out(exec, dir, &["add", "-A"]);
    git_out(
        exec,
        dir,
        &[
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            message,
        ],
    );
}

fn tree_paths(exec: &dyn GitExec, dir: &Path) -> Vec<String> {
    git_out(exec, dir, &["ls-tree", "-r", "--name-only", "HEAD"])
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// A note whose body carries `content`, with a description that passes on its own.
fn leak_note(content: &str) -> String {
    format!("---\ndescription: Leak\n---\n\n{content}")
}

fn validate(fixture: &Fixture) -> CompletionValidation {
    validate_completion(
        &fixture.worktree,
        &fixture.worktree.base_commit_sha,
        fixture.exec.as_ref(),
    )
}

#[test]
fn given_an_uncommitted_worktree_edit_when_validated_then_it_reports_dirty_uncommitted() {
    let fixture = fixture(&[], "run-dirty");
    std::fs::write(fixture.worktree.dir.join("dirty.txt"), "dirty content\n").expect("write dirty");

    assert!(matches!(
        validate(&fixture),
        CompletionValidation::DirtyUncommitted { .. }
    ));
}

#[test]
fn given_altered_git_administration_file_when_validated_then_validation_fails() {
    let fixture = fixture(&[], "run-tamper");
    let git_file = &fixture.worktree.git_file_path;
    let original = std::fs::read_to_string(git_file).expect("read git file");
    std::fs::write(git_file, format!("{original}# tampered\n")).expect("tamper git file");

    match validate(&fixture) {
        CompletionValidation::Failed { detail } => {
            assert!(detail.contains("administration"), "{detail}");
        }
        other => panic!("expected Failed completion, got {other:?}"),
    }
}

#[test]
fn given_clean_worktree_with_valid_commit_when_validated_then_validation_succeeds() {
    let fixture = fixture(&[], "run-valid");
    std::fs::write(fixture.worktree.dir.join("learned.md"), "learned facts\n").expect("write file");

    let child_repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: fixture.worktree.dir.clone(),
        agent_id: "agent-valid".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    child_repo
        .commit_write(
            &["learned.md"],
            "reflect: learned facts",
            &GitCommitAuthor {
                agent_id: "agent-valid".to_string(),
                author_name: "Valid Agent".to_string(),
                author_email: None,
            },
        )
        .expect("commit write");

    match validate(&fixture) {
        CompletionValidation::Valid {
            tip_sha,
            changed_paths,
            dropped,
        } => {
            assert_eq!(changed_paths, vec!["learned.md"]);
            assert!(dropped.is_empty());
            assert!(!tip_sha.is_empty());
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
}

#[test]
fn given_a_commit_with_malformed_frontmatter_when_validated_then_the_entry_is_dropped() {
    let fixture = fixture(&[], "run-frontmatter");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[(
            "system/notes.md",
            "---\ndescription: by default: verify\n---\nbody\n",
        )],
        "reflect: notes",
    );

    match validate(&fixture) {
        CompletionValidation::NoChanges {
            tip_sha, dropped, ..
        } => {
            assert_eq!(tip_sha, fixture.worktree.base_commit_sha);
            assert_eq!(dropped.len(), 1);
            assert_eq!(dropped[0].path, "system/notes.md");
            assert!(
                dropped[0].reason.contains("invalid frontmatter: "),
                "{}",
                dropped[0].reason
            );
            assert!(
                dropped[0].reason.contains("safe YAML plain scalar"),
                "{}",
                dropped[0].reason
            );
        }
        other => panic!("expected NoChanges completion, got {other:?}"),
    }
}

#[test]
fn given_a_body_only_edit_of_legacy_frontmatter_when_validated_then_it_passes() {
    let fixture = fixture(&[("system/legacy.md", "---\ndescription: true\n---\nfirst\n")], "run-legacy");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("system/legacy.md", "---\ndescription: true\n---\nsecond\n")],
        "reflect: body only",
    );

    match validate(&fixture) {
        CompletionValidation::Valid { changed_paths, .. } => {
            assert_eq!(changed_paths, vec!["system/legacy.md"])
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
}

#[test]
fn given_malformed_frontmatter_outside_the_content_set_when_validated_then_it_passes() {
    let fixture = fixture(&[], "run-scratch");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[(
            "notes/scratch.md",
            "---\ndescription: by default: verify\n---\nbody\n",
        )],
        "reflect: scratch",
    );

    match validate(&fixture) {
        CompletionValidation::Valid { changed_paths, .. } => {
            assert_eq!(changed_paths, vec!["notes/scratch.md"])
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
}

#[test]
fn given_a_good_note_and_a_note_with_a_pem_block_when_validated_then_the_good_note_is_kept() {
    let fixture = fixture(&[], "run-partial");
    let leak = leak_note(PEM);
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("reference/good.md", GOOD), ("reference/leak.md", leak.as_str())],
        "two notes",
    );

    match validate(&fixture) {
        CompletionValidation::Valid {
            changed_paths,
            dropped,
            ..
        } => {
            assert_eq!(changed_paths, vec!["reference/good.md"]);
            let paths: Vec<&str> = dropped.iter().map(|entry| entry.path.as_str()).collect();
            assert_eq!(paths, vec!["reference/leak.md"]);
            assert!(
                !describe_dropped_entries(&dropped).contains("MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSk"),
                "{dropped:?}"
            );
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
    let tree = tree_paths(fixture.exec.as_ref(), &fixture.worktree.dir);
    assert!(!tree.contains(&"reference/leak.md".to_string()), "{tree:?}");
    assert_eq!(
        git_out(fixture.exec.as_ref(), &fixture.worktree.dir, &["status", "--porcelain"]),
        ""
    );
}

#[test]
fn given_a_secret_only_in_an_earlier_commit_when_validated_then_the_history_carries_no_secret() {
    let fixture = fixture(&[], "run-transient");
    let secret = format!("---\ndescription: T\n---\n\n{PEM}");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("reference/good.md", GOOD), ("reference/tmp.md", secret.as_str())],
        "add",
    );
    git_out(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &["rm", "-q", "reference/tmp.md"],
    );
    git_out(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            "remove",
        ],
    );

    match validate(&fixture) {
        CompletionValidation::Valid { changed_paths, .. } => {
            assert_eq!(changed_paths, vec!["reference/good.md"]);
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
    let range = format!("{}..HEAD", fixture.worktree.base_commit_sha);
    assert_eq!(
        git_out(fixture.exec.as_ref(), &fixture.worktree.dir, &["rev-list", "--count", &range]).trim(),
        "1"
    );
}

#[test]
fn given_a_note_whose_description_is_over_1024_characters_when_validated_then_it_is_dropped() {
    let fixture = fixture(&[], "run-long");
    let long = format!("---\ndescription: {}\n---\n\nbody\n", "x".repeat(1100));
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("reference/good.md", GOOD), ("reference/long.md", long.as_str())],
        "notes",
    );

    match validate(&fixture) {
        CompletionValidation::Valid {
            changed_paths,
            dropped,
            ..
        } => {
            assert_eq!(changed_paths, vec!["reference/good.md"]);
            assert_eq!(dropped[0].path, "reference/long.md");
            assert!(
                dropped[0].reason.contains("exceeds 1024 characters"),
                "{}",
                dropped[0].reason
            );
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
}

#[test]
fn given_a_frontmatter_less_root_file_moved_into_reference_when_validated_then_the_move_is_undone() {
    let fixture = fixture(&[("notes-dump.md", "# Title\n\nno frontmatter\n")], "run-move");
    std::fs::create_dir_all(fixture.worktree.dir.join("reference")).expect("reference dir");
    git_out(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &["mv", "notes-dump.md", "reference/notes-dump.md"],
    );
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("reference/good.md", GOOD)],
        "move and add",
    );

    match validate(&fixture) {
        CompletionValidation::Valid { changed_paths, .. } => {
            assert_eq!(changed_paths, vec!["reference/good.md"]);
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
    let tree = tree_paths(fixture.exec.as_ref(), &fixture.worktree.dir);
    assert!(tree.contains(&"notes-dump.md".to_string()), "{tree:?}");
    assert!(!tree.contains(&"reference/notes-dump.md".to_string()), "{tree:?}");
}

#[test]
fn given_a_repaired_tip_when_validated_again_then_the_dropped_entries_are_still_reported() {
    let fixture = fixture(&[], "run-repaired");
    let leak = leak_note(PEM);
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[
            ("reference/good.md", GOOD),
            ("reference/leak.md", leak.as_str()),
            (
                "reference/token=abc123456.md",
                "---\ndescription: Named\n---\n\nclean\n",
            ),
        ],
        "notes with password=hunter2Abrx9 in the subject",
    );

    let first = validate(&fixture);
    let second = validate(&fixture);

    let (first_tip, first_dropped) = match &first {
        CompletionValidation::Valid {
            tip_sha, dropped, ..
        } => (tip_sha.clone(), dropped.clone()),
        other => panic!("expected Valid completion, got {other:?}"),
    };
    assert_eq!(first_dropped.len(), 2);
    match &second {
        CompletionValidation::Valid {
            tip_sha, dropped, ..
        } => {
            assert_eq!(tip_sha, &first_tip);
            let mut paths: Vec<&str> = dropped.iter().map(|entry| entry.path.as_str()).collect();
            paths.sort_unstable();
            assert_eq!(paths, vec!["reference/***", "reference/leak.md"]);
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
    let message = git_out(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &["log", "-1", "--format=%B"],
    );
    assert!(
        message.contains("Omo-Completion-Dropped: {\"path\":\"reference/leak.md\",\"reason\":\"secret_like_content"),
        "{message}"
    );
    assert!(!message.contains("hunter2Abrx9"), "{message}");
    assert!(!message.contains("abc123456"), "{message}");
    assert!(
        !message.contains("MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSk"),
        "{message}"
    );
}

#[cfg(unix)]
#[test]
fn given_a_dropped_note_with_a_glob_pathspec_name_when_validated_then_only_that_note_is_dropped() {
    for name in [
        "reference/k*.md",
        "reference/k?ep1.md",
        "reference/k[e]ep1.md",
        ":!reference.md",
    ] {
        let fixture = fixture(
            &[("reference/keep1.md", GOOD), ("reference/keep2.md", GOOD)],
            "run-glob",
        );
        let leak = format!("---\ndescription: Leak\n---\n\n{PEM}");
        commit_files(
            fixture.exec.as_ref(),
            &fixture.worktree.dir,
            &[(name, leak.as_str()), ("reference/good.md", GOOD)],
            "glob name",
        );

        match validate(&fixture) {
            CompletionValidation::Valid {
                changed_paths,
                dropped,
                ..
            } => {
                assert_eq!(changed_paths, vec!["reference/good.md"], "{name}");
                let paths: Vec<&str> = dropped.iter().map(|entry| entry.path.as_str()).collect();
                assert_eq!(paths, vec![name]);
            }
            other => panic!("expected Valid completion for {name}, got {other:?}"),
        }
        let mut tree = tree_paths(fixture.exec.as_ref(), &fixture.worktree.dir);
        tree.sort_unstable();
        assert_eq!(
            tree,
            vec![
                "reference/good.md".to_string(),
                "reference/keep1.md".to_string(),
                "reference/keep2.md".to_string()
            ],
            "{name}"
        );
        assert_eq!(
            git_out(fixture.exec.as_ref(), &fixture.worktree.dir, &["status", "--porcelain"]),
            "",
            "{name}"
        );
    }
}

#[cfg(unix)]
#[test]
fn given_an_existing_glob_named_note_that_gets_a_secret_when_validated_then_the_other_edit_is_kept() {
    let globbed = "reference/k?ep*.md";
    let fixture = fixture(&[(globbed, GOOD), ("reference/keep1.md", GOOD)], "run-glob-edit");
    let edited = GOOD.replace("body", "edited body");
    let leak = format!("---\ndescription: Leak\n---\n\n{PEM}");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[(globbed, leak.as_str()), ("reference/keep1.md", edited.as_str())],
        "edit both",
    );

    match validate(&fixture) {
        CompletionValidation::Valid { changed_paths, .. } => {
            assert_eq!(changed_paths, vec!["reference/keep1.md"]);
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
    assert_eq!(
        git_out(
            fixture.exec.as_ref(),
            &fixture.worktree.dir,
            &["show", "HEAD:reference/keep1.md"]
        )
        .trim_end(),
        edited.trim_end()
    );
}

#[test]
fn given_a_valid_glob_named_note_when_the_agent_deletes_it_then_the_deletion_is_kept() {
    let fixture = fixture(
        &[("reference/k[e]p.md", GOOD), ("reference/keep1.md", GOOD)],
        "run-glob-delete",
    );
    git_out(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &["--literal-pathspecs", "rm", "-q", "--", "reference/k[e]p.md"],
    );
    git_out(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            "delete it",
        ],
    );

    match validate(&fixture) {
        CompletionValidation::Valid {
            changed_paths,
            dropped,
            ..
        } => {
            assert_eq!(changed_paths, vec!["reference/k[e]p.md"]);
            assert!(dropped.is_empty(), "{dropped:?}");
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
    assert_eq!(
        tree_paths(fixture.exec.as_ref(), &fixture.worktree.dir),
        vec!["reference/keep1.md".to_string()]
    );
}

#[test]
fn given_a_new_glob_named_note_without_frontmatter_when_validated_then_it_is_dropped() {
    let fixture = fixture(&[], "run-glob-bare");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[
            ("reference/k[x].md", "no frontmatter here\n"),
            ("reference/good.md", GOOD),
        ],
        "bare glob note",
    );

    match validate(&fixture) {
        CompletionValidation::Valid {
            changed_paths,
            dropped,
            ..
        } => {
            assert_eq!(changed_paths, vec!["reference/good.md"]);
            let paths: Vec<&str> = dropped.iter().map(|entry| entry.path.as_str()).collect();
            assert_eq!(paths, vec!["reference/k[x].md"]);
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
}

/// The drop runs through pathspecs the agent chose, so every repair call must pin the literal-pathspec
/// environment; a user's own glob/icase setting must not turn one name into many.
type RecordedGitCalls = Vec<(Vec<String>, BTreeMap<String, String>)>;
struct EnvRecordingExec {
    inner: Arc<dyn GitExec>,
    calls: Mutex<RecordedGitCalls>,
}

impl GitExec for EnvRecordingExec {
    fn run(&self, argv: &[String], options: &GitExecOptions) -> std::io::Result<GitExecResult> {
        self.calls
            .lock()
            .expect("record calls")
            .push((argv.to_vec(), options.env.clone()));
        self.inner.run(argv, options)
    }

    fn run_bytes(
        &self,
        argv: &[String],
        options: &GitExecOptions,
    ) -> std::io::Result<GitExecBytesResult> {
        self.inner.run_bytes(argv, options)
    }
}

#[test]
fn given_a_dropped_note_when_the_branch_is_rewritten_then_every_pathspec_call_is_literal() {
    let fixture = fixture(&[], "run-env");
    let leak = leak_note(PEM);
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("reference/leak.md", leak.as_str()), ("reference/good.md", GOOD)],
        "icase env",
    );
    let recording = EnvRecordingExec {
        inner: Arc::clone(&fixture.exec),
        calls: Mutex::new(Vec::new()),
    };

    let validation = validate_completion(
        &fixture.worktree,
        &fixture.worktree.base_commit_sha,
        &recording,
    );

    match validation {
        CompletionValidation::Valid { changed_paths, .. } => {
            assert_eq!(changed_paths, vec!["reference/good.md"]);
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
    let calls = recording.calls.lock().expect("calls");
    let repair_calls: Vec<&(Vec<String>, BTreeMap<String, String>)> = calls
        .iter()
        .filter(|(argv, _)| {
            argv.iter()
                .any(|arg| matches!(arg.as_str(), "reset" | "checkout" | "rm"))
        })
        .collect();
    assert!(!repair_calls.is_empty(), "the branch must have been rewritten");
    for (argv, env) in repair_calls {
        assert_eq!(
            env.get("GIT_LITERAL_PATHSPECS").map(String::as_str),
            Some("1"),
            "{argv:?}"
        );
        assert_eq!(
            env.get("GIT_GLOB_PATHSPECS").map(String::as_str),
            Some("0"),
            "{argv:?}"
        );
        assert_eq!(
            env.get("GIT_ICASE_PATHSPECS").map(String::as_str),
            Some("0"),
            "{argv:?}"
        );
        assert_eq!(
            env.get("GIT_NOGLOB_PATHSPECS").map(String::as_str),
            Some("0"),
            "{argv:?}"
        );
    }
}

#[test]
fn given_a_secret_shaped_file_name_when_the_drop_is_described_then_the_note_never_prints_it() {
    let fixture = fixture(&[], "run-secret-name");
    let name = "reference/ghp_Ab3dEf5hJ7kL9mN1pQ3rS5tU7vW9xY1zB3C5.md";
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[(name, "---\ndescription: Named\n---\n\nclean\n"), ("reference/good.md", GOOD)],
        "secret name",
    );

    match validate(&fixture) {
        CompletionValidation::Valid { dropped, .. } => {
            let note = describe_dropped_entries(&dropped);
            assert!(note.contains("Dropped 1 invalid memory entry"), "{note}");
            assert!(!note.contains("Ab3dEf5hJ7kL9mN1pQ3rS5tU7vW9xY1zB3C5"), "{note}");
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn given_a_dropped_path_containing_a_pipe_when_validated_again_then_the_trailer_reads_it_back() {
    let fixture = fixture(&[], "run-pipe");
    let leak = leak_note(PEM);
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("reference/a | b.md", leak.as_str()), ("reference/good.md", GOOD)],
        "pipe name",
    );

    let _ = validate(&fixture);
    match validate(&fixture) {
        CompletionValidation::Valid { dropped, .. } => {
            let paths: Vec<&str> = dropped.iter().map(|entry| entry.path.as_str()).collect();
            assert_eq!(paths, vec!["reference/a | b.md"]);
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
}

#[test]
fn given_every_changed_note_is_invalid_when_validated_then_the_run_reports_no_changes() {
    let fixture = fixture(&[], "run-all-invalid");
    let leak = leak_note(PEM);
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("reference/leak.md", leak.as_str())],
        "leak",
    );

    match validate(&fixture) {
        CompletionValidation::NoChanges {
            tip_sha, dropped, ..
        } => {
            assert_eq!(tip_sha, fixture.worktree.base_commit_sha);
            let paths: Vec<&str> = dropped.iter().map(|entry| entry.path.as_str()).collect();
            assert_eq!(paths, vec!["reference/leak.md"]);
        }
        other => panic!("expected NoChanges completion, got {other:?}"),
    }
}

#[test]
fn given_a_skill_with_an_unquoted_colon_description_when_validated_then_the_entry_is_dropped() {
    let fixture = fixture(&[], "run-skill");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[(
            "skills/deploy/SKILL.md",
            "---\nname: deploy\ndescription: Use by default: verify\n---\n\nsteps\n",
        )],
        "reflect skill",
    );

    match validate(&fixture) {
        CompletionValidation::NoChanges { dropped, .. } => {
            assert_eq!(dropped.len(), 1);
            assert_eq!(dropped[0].path, "skills/deploy/SKILL.md");
            assert!(
                dropped[0].reason.contains("not a safe YAML plain scalar"),
                "{}",
                dropped[0].reason
            );
        }
        other => panic!("expected NoChanges completion, got {other:?}"),
    }
}

#[test]
fn given_a_reference_note_without_a_description_when_validated_then_the_entry_is_dropped() {
    let fixture = fixture(&[], "run-no-description");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("reference/note.md", "---\nkind: note\n---\n\nbody\n")],
        "reflect note",
    );

    match validate(&fixture) {
        CompletionValidation::NoChanges { dropped, .. } => {
            let paths: Vec<&str> = dropped.iter().map(|entry| entry.path.as_str()).collect();
            assert_eq!(paths, vec!["reference/note.md"]);
        }
        other => panic!("expected NoChanges completion, got {other:?}"),
    }
}

#[test]
fn given_a_legacy_file_whose_body_is_edited_when_validated_then_it_stays_valid() {
    let fixture = fixture(&[("reference/legacy.md", LEGACY)], "run-legacy-body");
    let edited = LEGACY.replace("legacy body", "edited body");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("reference/legacy.md", edited.as_str())],
        "reflect legacy",
    );

    assert!(matches!(validate(&fixture), CompletionValidation::Valid { .. }));
}

#[test]
fn given_a_legacy_file_whose_invalid_frontmatter_is_retouched_when_validated_then_it_is_dropped() {
    let fixture = fixture(&[("reference/legacy.md", LEGACY)], "run-legacy-retouch");
    let retouched = LEGACY.replace("verify, merge", "verify, test, merge");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[("reference/legacy.md", retouched.as_str())],
        "reflect legacy header",
    );

    match validate(&fixture) {
        CompletionValidation::NoChanges { dropped, .. } => {
            let paths: Vec<&str> = dropped.iter().map(|entry| entry.path.as_str()).collect();
            assert_eq!(paths, vec!["reference/legacy.md"]);
        }
        other => panic!("expected NoChanges completion, got {other:?}"),
    }
}

#[test]
fn given_strict_frontmatter_and_a_deletion_when_validated_then_it_is_valid() {
    let fixture = fixture(&[("reference/old.md", LEGACY)], "run-strict-delete");
    commit_files(
        fixture.exec.as_ref(),
        &fixture.worktree.dir,
        &[(
            "skills/deploy/SKILL.md",
            "---\nname: deploy\ndescription: \"Use by default: verify\"\nversion: 0.1.0\n---\n\nsteps\n",
        )],
        "reflect skill",
    );
    std::fs::remove_file(fixture.worktree.dir.join("reference/old.md")).expect("remove old");
    let child_repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: fixture.worktree.dir.clone(),
        agent_id: "agent-one".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open child repo");
    child_repo
        .commit_write(
            &["reference/old.md"],
            "forget",
            &GitCommitAuthor {
                agent_id: "agent-one".to_string(),
                author_name: "Reflection Agent".to_string(),
                author_email: None,
            },
        )
        .expect("commit write");

    match validate(&fixture) {
        CompletionValidation::Valid { changed_paths, .. } => {
            let mut sorted = changed_paths;
            sorted.sort_unstable();
            assert_eq!(sorted, vec!["reference/old.md", "skills/deploy/SKILL.md"]);
        }
        other => panic!("expected Valid completion, got {other:?}"),
    }
}

#[test]
fn given_a_tip_equal_to_the_base_when_validated_then_it_reports_no_changes() {
    let fixture = fixture(&[], "run-noop");

    match validate(&fixture) {
        CompletionValidation::NoChanges {
            tip_sha,
            changed_paths,
            dropped,
        } => {
            assert_eq!(tip_sha, fixture.worktree.base_commit_sha);
            assert!(changed_paths.is_empty());
            assert!(dropped.is_empty());
        }
        other => panic!("expected NoChanges completion, got {other:?}"),
    }
}
