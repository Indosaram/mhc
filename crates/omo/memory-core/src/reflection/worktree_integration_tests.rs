use pretty_assertions::assert_eq;

use tempfile::TempDir;

use crate::git::exec::system_git_exec;
use crate::git::{GitCommitAuthor, GitMemoryRepo, GitMemoryRepoOptions, InitializeGitRepoOptions};

use crate::reflection::completion_validation::validate_completion;
use crate::reflection::machine::ReflectionOutcome;
use crate::reflection::worktree::create_reflection_worktree;
use crate::reflection::worktree_integration::{
    IntegrateValidatedReflectionInput, ReflectionIntegrationMode, ReflectionIntegrationProbe,
    ValidatedReflectionTip, cleanup_reflection_worktree, integrate_validated_reflection,
    probe_reflection_integration,
};

#[test]
fn given_a_validated_tip_when_auto_integration_lands_then_it_creates_a_no_ff_receipt() {
    let temp = TempDir::new().expect("temp dir");
    let parent_dir = temp.path().join("parent");
    let worktrees_dir = temp.path().join("worktrees");

    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: parent_dir.clone(),
        agent_id: "agent-integration".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    repo.init(InitializeGitRepoOptions {
        seed_files: Vec::new(),
        author_name: Some("Integration Agent".to_string()),
        install_hooks: None,
    })
    .expect("repo init");

    let exec = system_git_exec();
    let worktree =
        create_reflection_worktree(&repo, "run-auto-land", &worktrees_dir, exec.as_ref(), None)
            .expect("create worktree");

    let file_path = worktree.dir.join("learned.md");
    std::fs::write(&file_path, "learned knowledge\n").expect("write file");

    let child_repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: worktree.dir.clone(),
        agent_id: "agent-integration".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    child_repo
        .commit_write(
            &["learned.md"],
            "learned knowledge",
            &GitCommitAuthor {
                agent_id: "agent-integration".to_string(),
                author_name: "Integration Agent".to_string(),
                author_email: None,
            },
        )
        .expect("child commit");

    let validation = validate_completion(&worktree, &worktree.base_commit_sha, exec.as_ref());
    let (tip_sha, changed_paths) = match validation {
        crate::reflection::completion_validation::CompletionValidation::Valid {
            tip_sha,
            changed_paths,
        } => (tip_sha, changed_paths),
        _ => panic!("Expected valid completion"),
    };

    let input = IntegrateValidatedReflectionInput {
        mode: ReflectionIntegrationMode::Auto,
        run_id: "run-auto-land".to_string(),
        summary: "learned knowledge".to_string(),
        validated: ValidatedReflectionTip {
            tip_sha,
            changed_paths,
        },
    };

    let result = integrate_validated_reflection(&worktree, input, |op| op());
    assert_eq!(result.outcome, ReflectionOutcome::Merged);
    assert!(result.integration_sha.is_some());

    let probe = probe_reflection_integration(
        &worktree,
        ReflectionIntegrationMode::Auto,
        "run-auto-land",
        result.integration_sha.as_ref().unwrap(),
    );
    assert!(matches!(
        probe,
        ReflectionIntegrationProbe::AlreadyMerged { .. }
    ));

    let cleanup = cleanup_reflection_worktree(&worktree);
    assert!(cleanup.worktree_removed);
    assert!(cleanup.branch_removed);
}
