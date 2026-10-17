use std::path::Path;

use memory_core::git::{
    UnlockFindings, UnlockInspection, UnlockOutcome, UnlockVerdict, WriterState,
    inspect_stranded_git_lock, unlock_stranded_git_lock,
};

use super::types::{CommandContext, CommandResponse, MemoryCommandIdentity, NotifyLevel, respond};

fn pid_list(pids: Option<&[u32]>, unknown: &str) -> String {
    let Some(pids) = pids else {
        return unknown.to_owned();
    };
    if pids.is_empty() {
        "none".to_owned()
    } else {
        pids.iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn describe_writer(writer: &WriterState) -> String {
    match writer {
        WriterState::None => "none".to_owned(),
        WriterState::Alive { pid } => {
            if *pid == std::process::id() {
                format!("pid {pid} (this session's process)")
            } else {
                format!("pid {pid}")
            }
        }
        WriterState::Unknown => "could not be determined on this system".to_owned(),
    }
}

fn describe_findings(findings: &UnlockFindings) -> Vec<String> {
    vec![
        format!("Lock: {}", findings.lock_path.display()),
        format!(
            "- processes holding it open: {}",
            pid_list(
                findings.holders.as_deref(),
                "could not be determined on this system"
            )
        ),
        format!(
            "- processes working in the repository: {}",
            pid_list(
                findings.repo_users.as_deref(),
                "could not be determined on this system"
            )
        ),
        format!("- live memory writer: {}", describe_writer(&findings.writer)),
    ]
}

fn refusal(findings: &UnlockFindings) -> String {
    let reason = if findings.writer == WriterState::Unknown {
        let writer_lock_path = findings
            .writer_lock_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        format!(
            "Not removed: the memory writer could not be determined (its lock record {writer_lock_path} is unreadable, from another host, or its process cannot be checked). If no memory session is running, remove that record and run /memory unlock again."
        )
    } else {
        "Not removed: a live process is still using this lock. Let it finish (or stop it), then run /memory unlock again.".to_owned()
    };
    let mut lines = describe_findings(findings);
    lines.push(String::new());
    lines.push(reason);
    lines.join("\n")
}

pub async fn run_memory_unlock(
    ctx: &CommandContext,
    identity: &MemoryCommandIdentity,
    raw_path: &str,
    force: bool,
) -> CommandResponse {
    let repo_dir = &identity.identity_paths.repo;
    if raw_path.is_empty() {
        return respond(
            ctx,
            "usage: /memory unlock <lock path> [--force]",
            NotifyLevel::Error,
        );
    }
    let raw = Path::new(raw_path);
    let lock_path = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        repo_dir.join(raw)
    };

    let inspection = inspect_stranded_git_lock(repo_dir, &lock_path, None, None);
    let findings = match inspection {
        UnlockInspection::NotAGitLock => {
            return respond(
                ctx,
                format!(
                    "{} is not a git lock file of the memory repository at {}; nothing was removed",
                    lock_path.display(),
                    repo_dir.display()
                ),
                NotifyLevel::Error,
            );
        }
        UnlockInspection::Missing => {
            return respond(
                ctx,
                format!("{} no longer exists; nothing to unlock", lock_path.display()),
                NotifyLevel::Info,
            );
        }
        UnlockInspection::Found(findings) => findings,
    };

    if findings.verdict == UnlockVerdict::Refuse {
        return respond(ctx, refusal(&findings), NotifyLevel::Error);
    }

    let findings_text = describe_findings(&findings).join("\n");
    if !force {
        if !ctx.is_interactive() {
            return respond(
                ctx,
                format!(
                    "{findings_text}\n\nNo live process was found using this lock. Run /memory unlock {} --force to remove it.",
                    findings.lock_path.display()
                ),
                NotifyLevel::Info,
            );
        }
        let prompt_body = format!(
            "{findings_text}\n\nRemove only if no git command or other session is working on this memory."
        );
        let confirmed = ctx
            .ui
            .confirm("Remove stranded git lock?", &prompt_body)
            .await;
        if !confirmed {
            return respond(
                ctx,
                format!(
                    "{} was kept; nothing was removed",
                    findings.lock_path.display()
                ),
                NotifyLevel::Info,
            );
        }
    }

    let outcome = match unlock_stranded_git_lock(repo_dir, &findings, None) {
        Ok(outcome) => outcome,
        Err(error) => {
            return respond(
                ctx,
                format!(
                    "{} could not be removed: {error}",
                    findings.lock_path.display()
                ),
                NotifyLevel::Error,
            );
        }
    };

    match outcome {
        UnlockOutcome::Removed { lock_path } => respond(
            ctx,
            format!("Removed {}. Retry the memory write.", lock_path.display()),
            NotifyLevel::Info,
        ),
        UnlockOutcome::Refused { inspection } => {
            respond(ctx, refusal(&inspection), NotifyLevel::Error)
        }
        UnlockOutcome::Changed { lock_path } => respond(
            ctx,
            format!(
                "{} changed after it was checked (a git process took it again); it was kept. Run /memory unlock again once that process is done.",
                lock_path.display()
            ),
            NotifyLevel::Error,
        ),
        UnlockOutcome::Missing => respond(
            ctx,
            format!("{} no longer exists; nothing to unlock", lock_path.display()),
            NotifyLevel::Info,
        ),
        UnlockOutcome::NotAGitLock => respond(
            ctx,
            format!(
                "{} is no longer a git lock file of the memory repository; nothing was removed",
                lock_path.display()
            ),
            NotifyLevel::Error,
        ),
        UnlockOutcome::UnlinkFailed { lock_path, message } => respond(
            ctx,
            format!("{} could not be removed: {message}", lock_path.display()),
            NotifyLevel::Error,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;

    use memory_core::git::{
        GitCommitAuthor, GitExec, GitExecOptions, GitExecResult, GitMemoryRepo,
        GitMemoryRepoOptions, system_git_exec,
    };
    use memory_core::locks::memory_writer_lock_path;
    use memory_core::tools::{MemoryToolParams, run_memory_tool};

    use crate::commands::memory::handle_memory;
    use crate::commands::test_support::*;
    use crate::commands::types::NotifyLevel;

    struct ChildGuard(Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct Harness {
        _root: tempfile::TempDir,
        fake: FakeDeps,
        context: FakeContext,
        index_lock: PathBuf,
        ref_lock: PathBuf,
        locks_dir: PathBuf,
        repo_dir: PathBuf,
    }

    fn harness(has_ui: bool) -> Harness {
        let (root, identity) = temp_identity();
        seeded_repo(
            &identity,
            vec![seed(
                "system/persona.md",
                "---\ndescription: Persona\n---\nseeded persona\n",
            )],
        );
        let git_dir = identity.identity_paths.repo.join(".git");
        let index_lock = git_dir.join("index.lock");
        let ref_lock = git_dir.join("refs").join("heads").join("main.lock");
        std::fs::write(&index_lock, "").expect("write index.lock");
        std::fs::write(&ref_lock, "").expect("write main.lock");
        std::fs::create_dir_all(&identity.identity_paths.locks).expect("create locks dir");
        let locks_dir = identity.identity_paths.locks.clone();
        let repo_dir = identity.identity_paths.repo.clone();
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions {
            has_ui: Some(has_ui),
            ..Default::default()
        });
        Harness {
            _root: root,
            fake,
            context,
            index_lock,
            ref_lock,
            locks_dir,
            repo_dir,
        }
    }

    fn record_live_writer(locks_dir: &std::path::Path) -> ChildGuard {
        let child = Command::new("cat")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn live writer");
        let pid = child.id();
        let record = serde_json::json!({
            "pid": pid,
            "process_start": "unavailable",
            "hostname": memory_core::support::host::hostname(),
            "nonce": memory_core::support::random::random_uuid(),
            "created_at": memory_core::support::time::now_iso(),
            "purpose": "memory-write",
        });
        std::fs::write(
            memory_writer_lock_path(locks_dir),
            serde_json::to_string(&record).expect("json"),
        )
        .expect("write lock record");
        ChildGuard(child)
    }

    #[tokio::test]
    async fn given_an_unreadable_memory_writer_lock_record_when_unlock_runs_with_force_then_it_refuses_up_front_naming_the_record_and_the_next_step()
    {
        let h = harness(false);
        let writer_lock = memory_writer_lock_path(&h.locks_dir);
        std::fs::write(&writer_lock, "{not json").expect("write corrupt record");

        let response = handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {} --force", h.index_lock.display()),
        )
        .await;

        assert!(h.index_lock.exists());
        assert_eq!(h.context.ui.last_level(), Some(NotifyLevel::Error));
        assert!(response.text.contains(&writer_lock.display().to_string()));
    }

    #[tokio::test]
    async fn given_an_interactive_session_and_a_confirmed_dialog_when_unlock_runs_then_only_that_lock_is_removed()
    {
        let h = harness(true);

        handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {}", h.index_lock.display()),
        )
        .await;

        assert_eq!(h.context.ui.confirms().len(), 1);
        assert!(!h.index_lock.exists());
        assert!(h.ref_lock.exists());
    }

    #[tokio::test]
    async fn given_an_interactive_session_and_a_declined_dialog_when_unlock_runs_then_nothing_is_removed()
    {
        let h = harness(true);
        h.context.ui.set_confirm_result(false);

        let response = handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {}", h.index_lock.display()),
        )
        .await;

        assert_eq!(h.context.ui.confirms().len(), 1);
        assert!(h.index_lock.exists());
        assert!(response.text.contains("was kept; nothing was removed"));
    }

    #[tokio::test]
    async fn given_a_noninteractive_session_without_force_when_unlock_runs_then_nothing_is_removed_and_force_is_offered()
    {
        let h = harness(false);

        let response = handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {}", h.index_lock.display()),
        )
        .await;

        assert!(h.index_lock.exists());
        assert!(response.text.contains("--force"));
    }

    #[tokio::test]
    async fn given_a_noninteractive_session_with_force_when_unlock_runs_then_the_lock_is_removed_without_a_dialog()
    {
        let h = harness(false);

        handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {} --force", h.index_lock.display()),
        )
        .await;

        assert!(!h.index_lock.exists());
    }

    #[tokio::test]
    async fn given_a_live_memory_writer_when_unlock_runs_with_force_then_it_refuses_never_overriding_the_refusal()
    {
        let h = harness(false);
        let _writer = record_live_writer(&h.locks_dir);

        handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {} --force", h.index_lock.display()),
        )
        .await;

        assert!(h.index_lock.exists());
        assert_eq!(h.context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_a_live_memory_writer_in_an_interactive_session_when_unlock_runs_then_it_refuses_without_asking()
    {
        let h = harness(true);
        let _writer = record_live_writer(&h.locks_dir);

        handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {}", h.index_lock.display()),
        )
        .await;

        assert_eq!(h.context.ui.confirms().len(), 0);
        assert!(h.index_lock.exists());
        assert_eq!(h.context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_not_a_git_lock_or_missing_lock_when_unlock_runs_then_it_reports_the_matching_outcome()
    {
        let h = harness(false);
        let notes = h.repo_dir.join("notes.lock");
        std::fs::write(&notes, "").expect("write notes.lock");

        let not_a_lock = handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {} --force", notes.display()),
        )
        .await;
        assert!(not_a_lock.text.contains("is not a git lock file of the memory repository"));
        assert_eq!(not_a_lock.level, NotifyLevel::Error);

        std::fs::remove_file(&h.index_lock).expect("remove index.lock");
        let missing = handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {} --force", h.index_lock.display()),
        )
        .await;
        assert!(missing.text.contains("no longer exists; nothing to unlock"));
        assert_eq!(missing.level, NotifyLevel::Info);
    }

    #[tokio::test]
    async fn given_a_lock_that_changes_during_confirmation_when_unlock_runs_then_it_reports_changed_and_keeps_the_lock()
    {
        let h = harness(true);
        let lock_to_replace = h.index_lock.clone();
        h.context.ui.set_on_confirm(move || {
            let _ = std::fs::remove_file(&lock_to_replace);
            let _ = std::fs::write(&lock_to_replace, "new writer");
        });

        let response = handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {}", h.index_lock.display()),
        )
        .await;

        assert!(h.index_lock.exists());
        assert_eq!(response.level, NotifyLevel::Error);
        assert!(
            response
                .text
                .contains("changed after it was checked (a git process took it again); it was kept")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn given_an_unremovable_lock_when_unlock_runs_then_it_reports_unlink_failed() {
        use std::os::unix::fs::PermissionsExt;

        let h = harness(false);
        let heads_dir = h.ref_lock.parent().expect("heads dir").to_path_buf();
        std::fs::set_permissions(&heads_dir, std::fs::Permissions::from_mode(0o555))
            .expect("chmod 555");

        let response = handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {} --force", h.ref_lock.display()),
        )
        .await;

        std::fs::set_permissions(&heads_dir, std::fs::Permissions::from_mode(0o755))
            .expect("chmod 755");

        assert!(h.ref_lock.exists());
        assert_eq!(response.level, NotifyLevel::Error);
        assert!(response.text.contains("could not be removed:"));
    }

    #[tokio::test]
    async fn given_a_commit_time_index_lock_rollback_when_unlocked_via_command_then_the_next_write_unstages_and_commits()
    {
        struct IndexLockAtCommitExec {
            inner: Arc<dyn GitExec>,
            repo_dir: PathBuf,
        }

        impl GitExec for IndexLockAtCommitExec {
            fn run(
                &self,
                argv: &[String],
                options: &GitExecOptions,
            ) -> std::io::Result<GitExecResult> {
                if argv.first().map(String::as_str) == Some("commit") {
                    let _ = std::fs::write(self.repo_dir.join(".git").join("index.lock"), "");
                }
                self.inner.run(argv, options)
            }
        }

        let h = harness(false);
        std::fs::remove_file(&h.index_lock).expect("clear initial index.lock");
        std::fs::remove_file(&h.ref_lock).expect("clear initial ref.lock");

        let author = GitCommitAuthor {
            agent_id: "test-identity".to_owned(),
            author_name: "Test Agent".to_owned(),
            author_email: None,
        };
        let locked_repo = GitMemoryRepo::new(GitMemoryRepoOptions {
            dir: h.repo_dir.clone(),
            agent_id: author.agent_id.clone(),
            exec: Some(Arc::new(IndexLockAtCommitExec {
                inner: system_git_exec(),
                repo_dir: h.repo_dir.clone(),
            })),
            install_hooks: None,
        })
        .expect("locked repo");

        let failure = run_memory_tool(
            &locked_repo,
            &author,
            &MemoryToolParams {
                command: Some("create".to_owned()),
                reason: "write behind commit-time index.lock".to_owned(),
                file_path: Some("system/new.md".to_owned()),
                description: Some("Fact".to_owned()),
                file_text: Some("new fact".to_owned()),
                ..Default::default()
            },
            None,
        )
        .expect_err("commit fails behind index.lock");
        assert!(failure.message.contains("/memory unlock"));
        assert!(failure.message.contains("still staged"));

        let unlock_res = handle_memory(
            &h.fake.deps,
            &h.context.ctx,
            &format!("unlock {} --force", h.index_lock.display()),
        )
        .await;
        assert_eq!(unlock_res.level, NotifyLevel::Info);
        assert!(unlock_res.text.contains("Removed"));
        assert!(!h.index_lock.exists());

        let clean_repo = GitMemoryRepo::open(&h.repo_dir, &author.agent_id).expect("open repo");
        let retried = run_memory_tool(
            &clean_repo,
            &author,
            &MemoryToolParams {
                command: Some("create".to_owned()),
                reason: "write after unlock".to_owned(),
                file_path: Some("system/other.md".to_owned()),
                description: Some("Fact".to_owned()),
                file_text: Some("other fact".to_owned()),
                ..Default::default()
            },
            None,
        )
        .expect("retry succeeds");
        assert_eq!(
            retried.commit.expect("commit").affected_paths,
            vec!["system/other.md".to_owned()]
        );
        assert!(!h.repo_dir.join("system/new.md").exists());
        assert!(h.repo_dir.join("system/other.md").exists());
    }
}
