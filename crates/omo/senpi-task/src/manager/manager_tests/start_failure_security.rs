//! `manager/start-failure-security.test.ts` (manager surfaces).
//!
//! The task-tool rendering and team-spawn halves of the TS file belong to the
//! slice-C `tools`/`team` ports. Rust's closed `ManagedRunnerError` enum cannot
//! be spoofed or carry hostile accessors, so those TS cases collapse onto the
//! `Other` (unknown error) variant.

use std::sync::Arc;

use serde_json::json;

use super::fakes::{FakeRunner, Harness, HarnessOptions, TeamPortManager, base_spec, make_manager};
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{
    ChildPlanner, ManagedRunnerError, ManagerStartSpec, ResolvedChildPlan, StartFailure,
    StartResult,
};
use crate::runners::{RunnerFailure, RunnerFailureKind};
use crate::state::{ResolvedModelRecord, ResolvedModelSource};
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::spawn_members::{SpawnMembersInput, spawn_team_members};

const ADVERSARIAL_ERROR: &str =
    "ENOENT /Users/alice/.config/senpi/credentials.json api_key=sk-live-secret";
const GENERIC_START_FAILURE: &str = "Task runner failed to start.";

fn resolved_model(source: ResolvedModelSource) -> ResolvedModelRecord {
    ResolvedModelRecord {
        display: "GPT-5.6 Sol".to_string(),
        reasoning_effort: Some("xhigh".to_string()),
        ..ResolvedModelRecord::new(source, "openai", "gpt-5.6-sol")
    }
}

fn harness(error: ManagedRunnerError, plan: Option<ResolvedChildPlan>) -> Harness {
    let runner = FakeRunner::new();
    *super::fakes::lock(&runner.start_error) = Some(error);
    let planner: Option<ChildPlanner> = plan.map(|plan| {
        let planner: ChildPlanner = Arc::new(move |_spec: &ManagerStartSpec| Ok(plan.clone()));
        planner
    });
    make_manager(HarnessOptions {
        planner,
        in_process: Some(runner),
        ..HarnessOptions::default()
    })
}

fn start_failed(result: StartResult) -> StartFailure {
    match result {
        StartResult::StartFailed(failure) => failure,
        other => panic!("expected start_failed, got {other:?}"),
    }
}

#[test]
fn given_unknown_start_error_with_secrets_when_start_fails_then_persisted_surfaces_get_classification()
 {
    let model = resolved_model(ResolvedModelSource::Category);
    let harness = harness(
        ManagedRunnerError::Other(ADVERSARIAL_ERROR.to_string()),
        Some(ResolvedChildPlan {
            model: "openai/gpt-5.6-sol".to_string(),
            resolved_model: Some(model.clone()),
            category: Some("ultrabrain".to_string()),
            ..ResolvedChildPlan::default()
        }),
    );

    let result = harness.manager.start(&ManagerStartSpec {
        prompt: "private prompt payload".to_string(),
        category: Some("ultrabrain".to_string()),
        name: Some("secure-bg".to_string()),
        run_in_background: true,
        ..base_spec()
    });
    let debug = format!("{result:?}");
    let failure = start_failed(result);

    assert_eq!(failure.name, "secure-bg");
    assert_eq!(failure.category.as_deref(), Some("ultrabrain"));
    assert_eq!(failure.execution_mode, ExecutionMode::InProcess);
    assert_eq!(failure.model, "openai/gpt-5.6-sol");
    assert_eq!(failure.resolved_model.as_ref(), Some(&model));
    assert!(failure.run_in_background);
    assert_eq!(failure.error_message, GENERIC_START_FAILURE);

    let persisted = harness
        .store
        .load(&failure.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(
        persisted.error_message.as_deref(),
        Some(GENERIC_START_FAILURE)
    );
    let state_dir = harness.store.state_dir();
    let event_log = std::fs::read_to_string(
        state_dir
            .join("logs")
            .join(format!("{}.jsonl", failure.task_id)),
    )
    .expect("event log");
    assert!(
        event_log.contains(&format!(
            r#"{{"type":"task_start_failed","payload":{{"error_message":"{GENERIC_START_FAILURE}"}}}}"#
        )),
        "{event_log}"
    );
    let record_file = std::fs::read_to_string(
        state_dir
            .join("tasks")
            .join(format!("{}.json", failure.task_id)),
    )
    .expect("record file");
    for surface in [&debug, &event_log, &record_file] {
        assert!(!surface.contains(ADVERSARIAL_ERROR), "{surface}");
    }
}

#[test]
fn given_non_runner_error_imitating_runner_failure_when_start_fails_then_generic_classification() {
    let harness = harness(
        ManagedRunnerError::Other(format!("RunnerError depth-exceeded: {ADVERSARIAL_ERROR}")),
        None,
    );

    let failure = start_failed(harness.manager.start(&ManagerStartSpec {
        prompt: "private prompt payload".to_string(),
        category: Some("quick".to_string()),
        ..base_spec()
    }));

    assert_eq!(failure.error_message, GENERIC_START_FAILURE);
    assert!(!format!("{failure:?}").contains(ADVERSARIAL_ERROR));
}

#[test]
fn given_non_runner_error_with_secrets_when_team_member_spawn_fails_then_only_stable_classification()
{
    let runner = FakeRunner::new();
    *super::fakes::lock(&runner.start_error) =
        Some(ManagedRunnerError::Other(ADVERSARIAL_ERROR.to_string()));
    let harness = make_manager(HarnessOptions {
        process: Some(runner),
        ..HarnessOptions::default()
    });
    let spec = normalize_senpi_team_spec(
        &json!({
            "members": [{ "name": "alpha", "kind": "category", "category": "quick", "prompt": "work" }]
        }),
        "secure-team",
        None,
    )
    .expect("team spec normalizes");
    let manager = TeamPortManager::new(harness.manager.clone());
    let now = || 0i64;

    let result = spawn_team_members(&SpawnMembersInput {
        spec: &spec,
        team_run_id: "team-run-secret-test",
        manager: &manager,
        lead_session_id: "lead-session",
        spawn_depth: 1,
        max_parallel: 1,
        deadline_at: 1_000,
        now: &now,
        member_extension: None,
    });

    assert_eq!(result.spawned.len(), 0);
    let failure = result.failure.expect("team spawn failure");
    assert_eq!(
        failure.message,
        format!("member 'alpha' failed to start: {GENERIC_START_FAILURE}")
    );
    assert!(!failure.message.contains(ADVERSARIAL_ERROR));
}

#[test]
fn given_classified_runner_errors_with_secrets_when_named_subagent_start_fails_then_context_kept() {
    for (kind, public_message) in [
        (
            RunnerFailureKind::DepthExceeded,
            "In-process child depth limit exceeded.",
        ),
        (
            RunnerFailureKind::SessionCreateFailed,
            "In-process child session creation failed.",
        ),
        (
            RunnerFailureKind::ChildPromptFailed,
            "Child prompt failed to start.",
        ),
    ] {
        let model = resolved_model(ResolvedModelSource::Explicit);
        let harness = harness(
            ManagedRunnerError::Runner(RunnerFailure::new(kind, ADVERSARIAL_ERROR)),
            Some(ResolvedChildPlan {
                model: "openai/gpt-5.6-sol".to_string(),
                resolved_model: Some(model.clone()),
                agent_type: Some("momus".to_string()),
                ..ResolvedChildPlan::default()
            }),
        );

        let failure = start_failed(harness.manager.start(&ManagerStartSpec {
            prompt: "private prompt payload".to_string(),
            subagent_type: Some("momus".to_string()),
            name: Some("secure-agent".to_string()),
            run_in_background: false,
            ..base_spec()
        }));

        assert_eq!(failure.name, "secure-agent");
        assert_eq!(failure.subagent_type.as_deref(), Some("momus"));
        assert_eq!(failure.execution_mode, ExecutionMode::InProcess);
        assert_eq!(failure.model, "openai/gpt-5.6-sol");
        assert_eq!(failure.resolved_model.as_ref(), Some(&model));
        assert!(!failure.run_in_background);
        assert_eq!(failure.error_message, public_message, "{kind:?}");
        assert!(!format!("{failure:?}").contains(ADVERSARIAL_ERROR));
    }
}
