use pretty_assertions::assert_eq;

use std::path::PathBuf;
use tempfile::TempDir;

use crate::git::exec::system_git_exec;
use crate::git::{
    GitCommitAuthor, GitMemoryRepo, GitMemoryRepoOptions, GitMergeOptions, GitSeedFile,
    InitializeGitRepoOptions,
};

use crate::reflection::completion_validation::validate_completion;
use crate::reflection::machine::ReflectionOutcome;
use crate::reflection::worktree::{ReflectionWorktree, create_reflection_worktree};
use crate::reflection::worktree_integration::{
    IntegrateValidatedReflectionInput, LegacyAutoRunReceiptProbe, ReflectionIntegrationMode,
    ReflectionIntegrationProbe, ValidatedReflectionTip, cleanup_reflection_worktree,
    integrate_validated_reflection, probe_legacy_auto_run_receipt, probe_reflection_integration,
};

struct TestFixture {
    _temp: TempDir,
    parent_dir: PathBuf,
    worktrees_dir: PathBuf,
    repo: GitMemoryRepo,
    worktree: ReflectionWorktree,
}

fn fixture(run_id: &str) -> TestFixture {
    let temp = TempDir::new().expect("temp dir");
    let parent_dir = temp.path().join("parent");
    let worktrees_dir = temp.path().join("worktrees");

    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: parent_dir.clone(),
        agent_id: "agent-one".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    repo.init(InitializeGitRepoOptions {
        seed_files: vec![GitSeedFile {
            relative_path: "memory.md".to_string(),
            content: "base\n".to_string(),
        }],
        author_name: Some("Integration Agent".to_string()),
        install_hooks: None,
    })
    .expect("repo init");

    let exec = system_git_exec();
    let worktree = create_reflection_worktree(&repo, run_id, &worktrees_dir, exec.as_ref(), None)
        .expect("create worktree");

    TestFixture {
        _temp: temp,
        parent_dir,
        worktrees_dir,
        repo,
        worktree,
    }
}

fn commit_in_worktree(worktree: &ReflectionWorktree, path: &str, content: &str) {
    let file_path = worktree.dir.join(path);
    std::fs::write(&file_path, content).expect("write file");

    let child_repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: worktree.dir.clone(),
        agent_id: "agent-one".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");

    child_repo
        .commit_write(
            &[path],
            "reflect",
            &GitCommitAuthor {
                agent_id: "agent-one".to_string(),
                author_name: "Reflection Agent".to_string(),
                author_email: None,
            },
        )
        .expect("child commit");
}

fn validated_tip(worktree: &ReflectionWorktree) -> ValidatedReflectionTip {
    let exec = system_git_exec();
    let validation = validate_completion(worktree, worktree.base_sha(), exec.as_ref());
    match validation {
        crate::reflection::completion_validation::CompletionValidation::Valid {
            tip_sha,
            changed_paths,
            ..
        } => ValidatedReflectionTip {
            tip_sha,
            changed_paths,
        },
        _ => panic!("Expected valid completion"),
    }
}

#[test]
fn given_a_validated_tip_when_auto_integration_lands_then_it_creates_a_no_ff_receipt() {
    let fix = fixture("run-123");
    commit_in_worktree(&fix.worktree, "learned.md", "learned\n");
    let validation = validated_tip(&fix.worktree);

    let input = IntegrateValidatedReflectionInput {
        mode: ReflectionIntegrationMode::Auto,
        run_id: "run-123".to_string(),
        summary: "learned facts".to_string(),
        validated: validation,
    };

    let result = integrate_validated_reflection(&fix.worktree, input, |op| op());
    assert_eq!(result.outcome, ReflectionOutcome::Merged);

    let exec = system_git_exec();
    let head = exec
        .run_in(&fix.parent_dir, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout
        .trim()
        .to_string();
    assert_eq!(result.integration_sha.as_deref(), Some(head.as_str()));

    let parents = exec
        .run_in(
            &fix.parent_dir,
            &["rev-list", "--parents", "-n", "1", "HEAD"],
        )
        .unwrap()
        .stdout
        .split_whitespace()
        .map(String::from)
        .collect::<Vec<_>>();
    assert_eq!(parents.len(), 3);

    let body = exec
        .run_in(&fix.parent_dir, &["show", "-s", "--format=%B", "HEAD"])
        .unwrap()
        .stdout;
    assert!(body.contains("Omo-Run: run-123"));
    assert!(fix.worktree.dir.exists());

    let branch_ref = exec.run_in(
        &fix.parent_dir,
        &[
            "show-ref",
            "--verify",
            &format!("refs/heads/{}", fix.worktree.branch),
        ],
    );
    assert_eq!(branch_ref.unwrap().code, 0);
}

#[test]
fn given_legacy_auto_receipts_when_probed_without_a_validated_tip_then_only_the_exact_trailer_is_accepted()
 {
    let fix = fixture("legacy");
    let exec = system_git_exec();

    let non_exact = exec.run_in(
        &fix.parent_dir,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "non-exact",
            "-m",
            "Omo-Run: legacy-extra",
        ],
    );
    assert_eq!(non_exact.unwrap().code, 0);
    assert_eq!(
        probe_legacy_auto_run_receipt(&fix.worktree, "legacy"),
        LegacyAutoRunReceiptProbe::Incomplete
    );

    let exact = exec.run_in(
        &fix.parent_dir,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "exact",
            "-m",
            "Omo-Run: legacy",
        ],
    );
    assert_eq!(exact.unwrap().code, 0);
    let head = exec
        .run_in(&fix.parent_dir, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout
        .trim()
        .to_string();

    assert_eq!(
        probe_legacy_auto_run_receipt(&fix.worktree, "legacy"),
        LegacyAutoRunReceiptProbe::AlreadyRecorded {
            integration_sha: Some(head),
        }
    );
}

#[test]
fn given_a_landed_auto_merge_when_probed_then_exact_receipt_and_validated_tip_ancestry_prove_integration()
 {
    let fix = fixture("receipt");
    commit_in_worktree(&fix.worktree, "receipt.md", "receipt\n");
    let validation = validated_tip(&fix.worktree);
    let tip_sha = validation.tip_sha.clone();

    let merged = integrate_validated_reflection(
        &fix.worktree,
        IntegrateValidatedReflectionInput {
            mode: ReflectionIntegrationMode::Auto,
            run_id: "receipt".to_string(),
            summary: "receipt proof".to_string(),
            validated: validation,
        },
        |op| op(),
    );
    assert_eq!(merged.outcome, ReflectionOutcome::Merged);
    let integration_sha = merged.integration_sha.expect("expected merge receipt");

    let probe = probe_reflection_integration(
        &fix.worktree,
        ReflectionIntegrationMode::Auto,
        "receipt",
        &tip_sha,
    );
    assert_eq!(
        probe,
        ReflectionIntegrationProbe::AlreadyMerged { integration_sha }
    );
}

#[test]
fn given_a_matching_receipt_that_excludes_the_validated_tip_when_probed_then_it_is_rejected() {
    let fix = fixture("false-receipt");
    commit_in_worktree(&fix.worktree, "unmerged.md", "unmerged\n");
    let validation = validated_tip(&fix.worktree);

    let exec = system_git_exec();
    let unrelated = exec.run_in(
        &fix.parent_dir,
        &[
            "commit",
            "--allow-empty",
            "-m",
            "unrelated",
            "-m",
            "Omo-Run: false-receipt",
        ],
    );
    assert_eq!(unrelated.unwrap().code, 0);

    let probe = probe_reflection_integration(
        &fix.worktree,
        ReflectionIntegrationMode::Auto,
        "false-receipt",
        &validation.tip_sha,
    );
    assert_eq!(probe, ReflectionIntegrationProbe::NotIntegrated);
}

#[test]
fn given_a_non_exact_receipt_on_an_integrated_tip_when_auto_mode_probes_then_only_ancestry_is_accepted()
 {
    let fix = fixture("exact");
    commit_in_worktree(&fix.worktree, "exact.md", "exact\n");
    let validation = validated_tip(&fix.worktree);

    let exec = system_git_exec();
    let merge = exec.run_in(
        &fix.parent_dir,
        &[
            "merge",
            "--no-ff",
            &validation.tip_sha,
            "-m",
            "manual integration",
            "-m",
            "Omo-Run: exact-extra",
        ],
    );
    assert_eq!(merge.unwrap().code, 0);
    let head = exec
        .run_in(&fix.parent_dir, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout
        .trim()
        .to_string();

    let probe = probe_reflection_integration(
        &fix.worktree,
        ReflectionIntegrationMode::Auto,
        "exact",
        &validation.tip_sha,
    );
    assert_eq!(
        probe,
        ReflectionIntegrationProbe::AlreadyMerged {
            integration_sha: head,
        }
    );
}

#[test]
fn given_external_integration_without_a_receipt_when_either_mode_probes_then_ancestry_proves_integration()
 {
    let fix = fixture("external");
    commit_in_worktree(&fix.worktree, "external.md", "external\n");
    let validation = validated_tip(&fix.worktree);

    let integration_sha = fix
        .repo
        .merge(
            &validation.tip_sha,
            Some(&GitMergeOptions {
                no_ff: Some(true),
                message: Some("manual integration".to_string()),
            }),
        )
        .expect("repo merge");

    let probe_auto = probe_reflection_integration(
        &fix.worktree,
        ReflectionIntegrationMode::Auto,
        "external",
        &validation.tip_sha,
    );
    assert_eq!(
        probe_auto,
        ReflectionIntegrationProbe::AlreadyMerged {
            integration_sha: integration_sha.clone(),
        }
    );

    let probe_integration = probe_reflection_integration(
        &fix.worktree,
        ReflectionIntegrationMode::Integration,
        "external",
        &validation.tip_sha,
    );
    assert_eq!(
        probe_integration,
        ReflectionIntegrationProbe::AlreadyMerged { integration_sha }
    );
}

#[test]
fn given_an_owned_interrupted_merge_when_auto_integration_retries_then_it_aborts_that_merge_and_lands_one_clean_merge()
 {
    let fix = fixture("owned");
    commit_in_worktree(&fix.worktree, "owned.md", "owned\n");
    let validation = validated_tip(&fix.worktree);

    let exec = system_git_exec();
    let merge_pending = exec.run_in(
        &fix.parent_dir,
        &["merge", "--no-ff", "--no-commit", &validation.tip_sha],
    );
    assert_eq!(merge_pending.unwrap().code, 0);

    let merge_head = exec
        .run_in(
            &fix.parent_dir,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .unwrap()
        .stdout
        .trim()
        .to_string();
    assert_eq!(merge_head, validation.tip_sha);

    let result = integrate_validated_reflection(
        &fix.worktree,
        IntegrateValidatedReflectionInput {
            mode: ReflectionIntegrationMode::Auto,
            run_id: "owned".to_string(),
            summary: "owned residue".to_string(),
            validated: validation,
        },
        |op| op(),
    );
    assert_eq!(result.outcome, ReflectionOutcome::Merged);

    let merge_head_after = exec.run_in(
        &fix.parent_dir,
        &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
    );
    assert_ne!(merge_head_after.unwrap().code, 0);

    let status = exec
        .run_in(&fix.parent_dir, &["status", "--porcelain"])
        .unwrap()
        .stdout;
    assert_eq!(status.trim(), "");
}

#[test]
fn given_foreign_merge_residue_when_auto_integration_runs_then_parent_index_and_merge_state_remain_untouched()
 {
    let fix = fixture("target");
    commit_in_worktree(&fix.worktree, "target.md", "target\n");
    let validation = validated_tip(&fix.worktree);

    let exec = system_git_exec();
    let foreign = create_reflection_worktree(
        &fix.repo,
        "foreign",
        &fix.worktrees_dir,
        exec.as_ref(),
        None,
    )
    .expect("create foreign worktree");
    commit_in_worktree(&foreign, "foreign.md", "foreign\n");
    let foreign_tip = exec
        .run_in(&foreign.dir, &["rev-parse", "HEAD"])
        .unwrap()
        .stdout
        .trim()
        .to_string();

    let merge_foreign = exec.run_in(
        &fix.parent_dir,
        &["merge", "--no-ff", "--no-commit", &foreign_tip],
    );
    assert_eq!(merge_foreign.unwrap().code, 0);

    let before_status = exec
        .run_in(&fix.parent_dir, &["status", "--porcelain=v2"])
        .unwrap()
        .stdout;
    let before_tree = exec
        .run_in(&fix.parent_dir, &["write-tree"])
        .unwrap()
        .stdout
        .trim()
        .to_string();

    let result = integrate_validated_reflection(
        &fix.worktree,
        IntegrateValidatedReflectionInput {
            mode: ReflectionIntegrationMode::Auto,
            run_id: "target".to_string(),
            summary: "must not interfere".to_string(),
            validated: validation,
        },
        |op| op(),
    );
    assert_eq!(result.outcome, ReflectionOutcome::ParentDirty);

    let merge_head = exec
        .run_in(
            &fix.parent_dir,
            &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        )
        .unwrap()
        .stdout
        .trim()
        .to_string();
    assert_eq!(merge_head, foreign_tip);

    let after_status = exec
        .run_in(&fix.parent_dir, &["status", "--porcelain=v2"])
        .unwrap()
        .stdout;
    assert_eq!(after_status, before_status);

    let after_tree = exec
        .run_in(&fix.parent_dir, &["write-tree"])
        .unwrap()
        .stdout
        .trim()
        .to_string();
    assert_eq!(after_tree, before_tree);
}

#[test]
fn given_present_then_absent_resources_when_cleanup_repeats_then_both_receipts_confirm_absence() {
    let fix = fixture("cleanup");

    let cleanup1 = cleanup_reflection_worktree(&fix.worktree);
    assert!(cleanup1.worktree_removed);
    assert!(cleanup1.branch_removed);

    let cleanup2 = cleanup_reflection_worktree(&fix.worktree);
    assert!(cleanup2.worktree_removed);
    assert!(cleanup2.branch_removed);

    assert!(!fix.worktree.dir.exists());

    let exec = system_git_exec();
    let branch_ref = exec.run_in(
        &fix.parent_dir,
        &[
            "show-ref",
            "--verify",
            &format!("refs/heads/{}", fix.worktree.branch),
        ],
    );
    assert_ne!(branch_ref.unwrap().code, 0);
}
