//! `/doctor` — deterministic memory health checks plus the skill frontmatter repair.
//! Port of `components/memory/commands/doctor.ts` at pin 77f3067f1.

use std::sync::Arc;

use maho_ext_api::ExtensionApi;

use super::doctor_checks::{
    CheckLevel, DoctorCheck, check_abandoned_runs, check_frontmatter, check_locks,
    check_reflection_health, check_repository, check_soul_seed, check_tokens, check_worktrees,
};
use super::facts_status::{
    ReadFactsOverviewInput, facts_remediation_hint, format_facts_advisory, read_facts_overview,
};
use super::skill_frontmatter::{
    format_skill_name_frontmatter_repair_report, repair_missing_skill_name_frontmatter,
};
use super::types::{
    command_context_from, finish, require_identity, respond, CommandContext, CommandResponse,
    MemoryCommandDeps, MemoryCommandIdentity, NotifyLevel,
};

fn level_order(level: CheckLevel) -> u8 {
    match level {
        CheckLevel::Ok => 0,
        CheckLevel::Warn => 1,
        CheckLevel::Fail => 2,
    }
}

fn worst_level(checks: &[DoctorCheck]) -> CheckLevel {
    checks.iter().fold(CheckLevel::Ok, |worst, check| {
        if level_order(check.level) > level_order(worst) {
            check.level
        } else {
            worst
        }
    })
}

fn check_facts(
    deps: &MemoryCommandDeps,
    identity: &MemoryCommandIdentity,
) -> Option<DoctorCheck> {
    let overview = read_facts_overview(ReadFactsOverviewInput {
        identity_paths: &identity.identity_paths,
        now_ms: deps.now_ms(),
    });
    let advisory = format_facts_advisory(&overview)?;
    let hint = facts_remediation_hint(&overview);
    let detail = format!(
        "{}{}",
        advisory.strip_prefix("facts: ").unwrap_or(&advisory),
        hint.map(|hint| format!("; {hint}")).unwrap_or_default()
    );
    Some(DoctorCheck {
        name: "facts".to_owned(),
        level: if overview.corrupt.is_none() { CheckLevel::Warn } else { CheckLevel::Fail },
        detail,
    })
}

pub async fn handle_doctor(
    deps: &MemoryCommandDeps,
    ctx: &CommandContext,
    _args: &str,
) -> CommandResponse {
    let identity = match require_identity(deps, ctx) {
        Ok(identity) => identity,
        Err(error) => return respond(ctx, error, NotifyLevel::Error),
    };

    let repository = check_repository(&identity);
    let mut checks = vec![repository];
    let mut extra: Vec<String> = Vec::new();

    if checks[0].level == CheckLevel::Ok {
        let warn_tokens = (deps.settings)()
            .ok()
            .and_then(|settings| settings["compile_warn_tokens"].as_u64())
            .unwrap_or(0) as usize;
        checks.extend(check_frontmatter(&identity.identity_paths.repo));
        checks.push(check_soul_seed(&identity.identity_paths.repo));
        checks.push(check_locks(deps, &identity.identity_paths.locks));
        checks.push(check_worktrees(deps, &identity));
        checks.push(check_abandoned_runs(&identity.identity_paths.reflection));
        checks.push(check_reflection_health(&identity.identity_paths.reflection, deps.now_ms()));
        checks.push(check_tokens(&identity.identity_paths.repo, warn_tokens));

        if let Some(facts) = check_facts(deps, &identity) {
            checks.push(facts);
        }

        let repaired = repair_missing_skill_name_frontmatter(Some(&identity.identity_paths.repo));
        let report = format_skill_name_frontmatter_repair_report(&repaired);
        extra.push(format!(
            "[info] skills: scanned {} skill file{}",
            repaired.scanned,
            if repaired.scanned == 1 { "" } else { "s" }
        ));
        extra.extend(
            report
                .split('\n')
                .filter(|line| !line.is_empty())
                .map(|line| format!("[info] skills: {line}")),
        );
    }

    let level = worst_level(&checks);
    let mut lines = vec![
        format!("# Memory doctor: {}", identity.identity),
        String::new(),
    ];
    lines.extend(
        checks
            .iter()
            .map(|check| format!("[{}] {}: {}", check.level.as_str(), check.name, check.detail)),
    );
    lines.extend(extra);
    match level {
        CheckLevel::Fail => {
            lines.push(String::new());
            lines.push("fix the failing checks above, then re-run /doctor".to_owned());
        }
        CheckLevel::Warn => {
            lines.push(String::new());
            lines.push("warnings do not block memory; re-run /doctor after addressing them".to_owned());
        }
        CheckLevel::Ok => {}
    }
    let notify = match level {
        CheckLevel::Fail => NotifyLevel::Error,
        CheckLevel::Warn => NotifyLevel::Warning,
        CheckLevel::Ok => NotifyLevel::Info,
    };
    respond(ctx, lines.join("\n"), notify)
}

pub fn register_doctor_command(api: &mut ExtensionApi, deps: Arc<MemoryCommandDeps>) {
    api.register_command(
        "doctor",
        Some("Run deterministic memory health checks and repair skill frontmatter.".to_owned()),
        Some(String::new()),
        Arc::new(move |args, context| {
            let deps = deps.clone();
            let context = command_context_from(context);
            let args = args.to_owned();
            Box::pin(async move { finish(handle_doctor(&deps, &context, &args).await) })
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::*;
    use memory_core::{
        facts::{
            failures_backoff::FactsFailureTarget,
            failures_schema::FactsFailureReason,
            failures_store::{FactsFailureStore, FactsFailureStoreOptions, RecordFailureRequest},
        },
        locks::memory_writer_lock_path,
    };

    fn seeds() -> Vec<memory_core::git::GitSeedFile> {
        vec![seed("system/persona.md", "---\ndescription: Persona\n---\nseeded persona\n")]
    }

    const V1_PERSONA_BODY: &str = concat!(
        "You are a coding agent that maintains its own memory.\n",
        "\n",
        "Your persistent context lives in a version-controlled memory filesystem rooted at $MEMORY_DIR. Files committed to HEAD are projected into your system prompt on the next run:\n",
        "\n",
        "- system/persona.md (this file) \u{2014} who you are and how you operate. Edit it to refine your own working identity.\n",
        "- system/human.md \u{2014} what you have learned about the person you work with. Update it as you discover durable preferences, context, and constraints.\n",
        "- system/*.md \u{2014} any other memory blocks you create under system/ are projected as nested XML.\n",
        "- Non-system paths (for example reference/ or notes/) appear as names in <external_projection> only; their bodies are never injected.\n",
        "\n",
        "Changes to these files only take effect after a git commit. Use the memory tools to edit, never hand-write raw git commands during a session. The system/ directory is your self-model; keep it accurate and minimal.",
    );

    fn harness(seeded: bool) -> (tempfile::TempDir, MemoryCommandIdentity, FakeDeps) {
        let (root, identity) = temp_identity();
        if seeded {
            seeded_repo(&identity, seeds());
        }
        let fake = fake_deps(Some(identity.clone()), FakeDepsOverrides::default());
        (root, identity, fake)
    }

    #[tokio::test]
    async fn given_a_healthy_repository_when_doctor_runs_then_every_deterministic_check_passes() {
        let (_root, _identity, fake) = harness(true);
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[ok] repository"));
        assert!(response.text.contains("[ok] frontmatter"));
        assert!(response.text.contains("[ok] locks"));
        assert!(response.text.contains("[ok] worktrees"));
        assert!(response.text.contains("[ok] tokens"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Info));
    }

    #[tokio::test]
    async fn given_a_persona_still_equal_to_the_v1_seed_when_doctor_runs_then_a_soul_seed_advisory_is_reported() {
        let (_root, identity) = temp_identity();
        let v1 = format!("---\ndescription: Persona - who I am\n---\n{}", V1_PERSONA_BODY);
        seeded_repo(&identity, vec![seed("system/persona.md", &v1)]);
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[warn] soul-seed"));
    }

    #[tokio::test]
    async fn given_a_persona_that_diverged_from_the_v1_seed_when_doctor_runs_then_no_soul_seed_advisory_fires() {
        let (_root, _identity, fake) = harness(true);
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[ok] soul-seed"));
    }

    #[tokio::test]
    async fn given_no_repository_when_doctor_runs_then_the_repository_check_fails_with_an_actionable_hint() {
        let (_root, _identity, fake) = harness(false);
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[fail] repository"));
        assert!(response.text.contains("/memfs init"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_a_memory_file_with_invalid_frontmatter_when_doctor_runs_then_the_sweep_fails_and_names_the_file() {
        let (_root, identity, fake) = harness(true);
        std::fs::write(identity.identity_paths.repo.join("system").join("broken.md"), "no frontmatter here\n")
            .expect("broken");
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[fail] frontmatter"));
        assert!(response.text.contains("system/broken.md"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }

    #[tokio::test]
    async fn given_a_missing_persona_file_when_doctor_runs_then_a_persona_warning_is_reported() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, vec![seed("external/notes.md", "notes\n")]);
        let fake = fake_deps(Some(identity), FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[warn] persona"));
        assert!(response.text.contains("system/persona.md"));
    }

    #[tokio::test]
    async fn given_a_lock_owned_by_a_dead_pid_when_doctor_runs_then_the_stale_lock_is_reported_with_its_path() {
        let (_root, identity, fake) = harness(true);
        std::fs::create_dir_all(&identity.identity_paths.locks).expect("locks dir");
        let lock_path = memory_writer_lock_path(&identity.identity_paths.locks);
        let record = serde_json::json!({
            "pid": 987654,
            "process_start": "start-token",
            "hostname": memory_core::support::host::hostname(),
            "nonce": "nonce-1",
            "created_at": "2026-08-16T00:00:00.000Z",
            "purpose": "memory-write",
        });
        std::fs::write(&lock_path, format!("{record}\n")).expect("lock");
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[warn] locks"));
        assert!(response.text.contains(&lock_path.display().to_string()));
        assert!(response.text.contains("987654"));
    }

    #[tokio::test]
    async fn given_a_lock_owned_by_a_live_pid_when_doctor_runs_then_no_stale_lock_is_reported() {
        let (_root, identity, fake) = harness(true);
        std::fs::create_dir_all(&identity.identity_paths.locks).expect("locks dir");
        let lock_path = memory_writer_lock_path(&identity.identity_paths.locks);
        let record = serde_json::json!({
            "pid": 4242,
            "process_start": "start-token",
            "hostname": memory_core::support::host::hostname(),
            "nonce": "nonce-1",
            "created_at": "2026-08-16T00:00:00.000Z",
            "purpose": "memory-write",
        });
        std::fs::write(&lock_path, format!("{record}\n")).expect("lock");
        alive_pids(&fake)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(4242);
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[ok] locks"));
    }

    #[tokio::test]
    async fn given_repeated_model_not_found_reflection_failures_when_doctor_runs_then_health_and_remediation_are_reported() {
        let (_root, identity, fake) = harness(true);
        let completions = identity.identity_paths.reflection.join("completions");
        std::fs::create_dir_all(&completions).expect("completions");
        for index in 0..3 {
            let record = serde_json::json!({
                "schemaVersion": 1,
                "runId": format!("run-{index}"),
                "identity": identity.identity,
                "category": "quick",
                "conversationIds": ["past-session"],
                "trigger": "manual",
                "outcome": "failed",
                "reason": "model-not-found",
                "detail": "configured model unavailable",
                "startedAt": format!("2026-08-12T0{index}:00:00.000Z"),
                "finishedAt": format!("2026-08-12T0{index}:01:00.000Z"),
                "delivery": { "status": if index == 2 { "pending" } else { "consumed" } },
            });
            std::fs::write(completions.join(format!("run-{index}.json")), format!("{record}\n"))
                .expect("health record");
        }
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[warn] reflection-health"));
        assert!(response.text.contains("streak 3"));
        assert!(response.text.contains("pending 1"));
        assert!(response.text.contains("adjust memory.reflection category/model in your omo config"));
    }

    #[tokio::test]
    async fn given_an_abandoned_reservation_run_when_doctor_runs_then_manual_disposal_paths_are_reported() {
        let (_root, identity, fake) = harness(true);
        let run_dir = identity.identity_paths.reflection.join("runs").join("run-abandoned");
        std::fs::create_dir_all(&run_dir).expect("run dir");
        let abandoned = serde_json::json!({
            "version": 1,
            "runId": "run-abandoned",
            "outcome": "abandoned_unknown",
            "abandonedAt": "2026-08-10T00:00:00.000Z",
        });
        std::fs::write(run_dir.join("abandoned.json"), format!("{abandoned}\n")).expect("abandoned");
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[warn] abandoned-runs"));
        assert!(response.text.contains(&run_dir.display().to_string()));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Warning));
    }

    #[tokio::test]
    async fn given_an_unregistered_worktree_directory_when_doctor_runs_then_the_orphan_is_reported() {
        let (_root, identity, fake) = harness(true);
        std::fs::create_dir_all(identity.identity_paths.worktrees.join("orphan-run")).expect("worktree");
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[warn] worktrees"));
        assert!(response.text.contains("orphan-run"));
    }

    #[tokio::test]
    async fn given_a_system_estimate_over_the_warn_threshold_when_doctor_runs_then_a_token_advisory_is_reported() {
        let (_root, identity) = temp_identity();
        seeded_repo(&identity, seeds());
        let mut settings = memory_settings();
        settings["compile_warn_tokens"] = serde_json::json!(1);
        let fake = fake_deps(
            Some(identity),
            FakeDepsOverrides { settings: Some(settings), ..Default::default() },
        );
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("[warn] tokens"));
        assert!(response.text.contains('1'));
    }

    #[tokio::test]
    async fn given_a_skill_missing_name_frontmatter_when_doctor_runs_then_the_repair_helper_reports_the_fix() {
        let (_root, identity, fake) = harness(true);
        std::fs::create_dir_all(identity.identity_paths.repo.join("skills").join("commit")).expect("dir");
        std::fs::write(
            identity.identity_paths.repo.join("skills").join("commit").join("SKILL.md"),
            "---\ndescription: x\n---\n",
        )
        .expect("skill");
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("skills/commit/SKILL.md"));
        assert!(response.text.contains("name:"));
    }

    #[tokio::test]
    async fn given_parked_facts_batches_when_doctor_runs_then_one_bounded_advisory_line_names_facts_retry() {
        let (_root, identity, fake) = harness(true);
        let store = FactsFailureStore::new(FactsFailureStoreOptions {
            identity_paths: identity.identity_paths.clone(),
            now: None,
            lock_wait_ms: None,
        });
        for attempt in 0..5 {
            store
                .record_failure(RecordFailureRequest {
                    targets: vec![FactsFailureTarget {
                        conversation_id: "conv-a".to_owned(),
                        end_message_id: "msg-a".to_owned(),
                        end_snapshot_line: 2,
                    }],
                    failure_id: format!("run-{attempt}"),
                    reason: FactsFailureReason::ChildExit,
                    detail: None,
                })
                .expect("record");
        }
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        let facts_lines: Vec<&str> = response
            .text
            .lines()
            .filter(|line| line.contains("] facts:"))
            .collect();
        assert_eq!(facts_lines.len(), 1);
        assert!(facts_lines[0].contains("1 parked"));
        assert!(facts_lines[0].contains("/facts retry"));
    }

    #[tokio::test]
    async fn given_no_facts_failures_when_doctor_runs_then_no_facts_advisory_line_is_rendered() {
        let (_root, _identity, fake) = harness(true);
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert_eq!(
            response.text.lines().filter(|line| line.contains("] facts:")).count(),
            0
        );
    }

    #[tokio::test]
    async fn given_an_unbound_session_when_doctor_runs_then_an_actionable_error_is_returned() {
        let fake = fake_deps(None, FakeDepsOverrides::default());
        let context = fake_command_context(FakeContextOptions::default());

        let response = handle_doctor(&fake.deps, &context.ctx, "").await;

        assert!(response.text.contains("not bound"));
        assert_eq!(context.ui.last_level(), Some(NotifyLevel::Error));
    }
}
