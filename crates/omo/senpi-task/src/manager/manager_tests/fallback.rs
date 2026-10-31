//! `manager/manager-fallback.test.ts` and `manager/manager-runtime-fallback.test.ts`.

use std::sync::Arc;

use super::fakes::{
    FakeHandle, FakeRunner, Harness, HarnessOptions, base_spec, make_manager, started,
    wait_terminal, wait_until,
};
use crate::category::{CategoryResolutionResult, ResolveCategoryOptions, resolve_category};
use crate::host::fake::{model, registry};
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{ChildPlanner, ManagerStartSpec, ResolvedChildPlan};
use crate::runners::{RunnerFailureKind, RunnerOutcome};
use crate::shared::ManagedChildEvent;
use crate::state::{
    ProvisionalExit, ResolvedModelRecord, ResolvedModelSource, TaskRecord, TaskStatus,
};

fn category_record(provider: &str, model_id: &str) -> ResolvedModelRecord {
    ResolvedModelRecord::new(ResolvedModelSource::Category, provider, model_id)
}

fn fallback_plan(fallbacks: Vec<ResolvedModelRecord>) -> ResolvedChildPlan {
    ResolvedChildPlan {
        model: "vendor-a/primary-model".to_string(),
        requested_model: Some(category_record("vendor-a", "primary-model")),
        resolved_model: Some(category_record("vendor-a", "primary-model")),
        fallback_models: Some(fallbacks),
        category: Some("quick".to_string()),
        ..ResolvedChildPlan::default()
    }
}

fn planner(plan: ResolvedChildPlan) -> ChildPlanner {
    Arc::new(move |_spec: &ManagerStartSpec| Ok(plan.clone()))
}

fn harness_with(plan: ResolvedChildPlan, runner: &Arc<FakeRunner>, mode: ExecutionMode) -> Harness {
    let slot = Some(Arc::clone(runner));
    make_manager(HarnessOptions {
        planner: Some(planner(plan)),
        in_process: matches!(mode, ExecutionMode::InProcess)
            .then(|| slot.clone())
            .flatten(),
        process: matches!(mode, ExecutionMode::Process)
            .then_some(slot)
            .flatten(),
        ..HarnessOptions::default()
    })
}

fn start_in(harness: &Harness, mode: ExecutionMode) -> String {
    started(harness.manager.start(&ManagerStartSpec {
        execution_mode: Some(mode),
        ..base_spec()
    }))
    .task_id
}

fn load(harness: &Harness, task_id: &str) -> TaskRecord {
    harness.store.load(task_id).expect("load").expect("record")
}

fn displays(models: Option<&Vec<ResolvedModelRecord>>) -> Vec<String> {
    models
        .map(|models| models.iter().map(|model| model.display.clone()).collect())
        .unwrap_or_default()
}

/// The handle of the `call`-th launch, once the manager subscribed to it.
fn launched(runner: &FakeRunner, task_id: &str, call: usize) -> Arc<FakeHandle> {
    wait_until("launch", || runner.started_count() >= call);
    let handle = runner.wait_handle(task_id);
    wait_until("subscribed", || handle.subscribe_count() > 0);
    handle
}

fn fallback_event(from: &str, to: &str) -> ManagedChildEvent {
    ManagedChildEvent {
        event_type: "retry_fallback_applied".to_string(),
        from: Some(from.to_string()),
        to: Some(to.to_string()),
        chain_key: Some(from.to_string()),
        reason: Some("hard-error".to_string()),
        ..ManagedChildEvent::default()
    }
}

// manager-fallback.test.ts

#[test]
fn given_provider_failure_before_any_tool_call_when_child_terminates_then_same_task_hands_off() {
    let runner = FakeRunner::new();
    let harness = harness_with(
        fallback_plan(vec![category_record("vendor-b", "fallback-model")]),
        &runner,
        ExecutionMode::InProcess,
    );
    let task_id = start_in(&harness, ExecutionMode::InProcess);
    let first = launched(&runner, &task_id, 1);
    let initial = load(&harness, &task_id);
    assert_eq!(initial.model, "vendor-a/primary-model");
    assert_eq!(
        displays(initial.fallback_models.as_ref()),
        vec!["vendor-b/fallback-model"]
    );

    first.fail(
        RunnerFailureKind::ChildTurnFailed,
        "provider capacity exhausted",
    );

    let second = launched(&runner, &task_id, 2);
    assert!(!Arc::ptr_eq(&first, &second));
    assert_eq!(
        runner.specs()[1].model.as_deref(),
        Some("vendor-b/fallback-model")
    );
    let handed_off = load(&harness, &task_id);
    assert_eq!(handed_off.status, TaskStatus::Running);
    assert_eq!(handed_off.model, "vendor-b/fallback-model");
    assert_eq!(
        handed_off.requested_model.map(|model| model.display),
        Some("vendor-a/primary-model".to_string())
    );
    assert_eq!(
        handed_off.resolved_model.map(|model| model.display),
        Some("vendor-b/fallback-model".to_string())
    );
    assert_eq!(handed_off.fallback_models, Some(Vec::new()));

    second.complete("completed after fallback");
    let terminal = wait_terminal(&harness.manager, &task_id);
    assert_eq!(terminal.status, TaskStatus::Completed);
    assert_eq!(terminal.model, "vendor-b/fallback-model");
    assert_eq!(
        terminal.final_response.as_deref(),
        Some("completed after fallback")
    );
}

#[test]
fn given_failed_child_already_executed_tool_when_terminates_then_no_replay() {
    let runner = FakeRunner::new();
    let harness = harness_with(
        fallback_plan(vec![category_record("vendor-b", "fallback-model")]),
        &runner,
        ExecutionMode::Process,
    );
    let task_id = start_in(&harness, ExecutionMode::Process);
    let handle = launched(&runner, &task_id, 1);
    handle.emit(&ManagedChildEvent {
        event_type: "tool_execution_start".to_string(),
        tool_name: Some("write".to_string()),
        args: Some(serde_json::json!({})),
        ..ManagedChildEvent::default()
    });

    handle.fail(
        RunnerFailureKind::ChildTurnFailed,
        "provider capacity exhausted",
    );

    assert_eq!(
        wait_terminal(&harness.manager, &task_id).status,
        TaskStatus::Error
    );
    assert_eq!(runner.started_count(), 1);
}

#[test]
fn given_every_configured_model_fails_before_tools_when_chain_exhausts_then_attempts_kept() {
    let runner = FakeRunner::new();
    let harness = harness_with(
        fallback_plan(vec![category_record("vendor-b", "fallback-model")]),
        &runner,
        ExecutionMode::Process,
    );
    let task_id = start_in(&harness, ExecutionMode::Process);
    let first = launched(&runner, &task_id, 1);
    first.fail(RunnerFailureKind::ChildPromptFailed, "primary unavailable");
    let second = launched(&runner, &task_id, 2);

    second.fail(RunnerFailureKind::ChildPromptFailed, "fallback unavailable");

    let terminal = wait_terminal(&harness.manager, &task_id);
    assert_eq!(terminal.status, TaskStatus::Error);
    assert_eq!(terminal.model, "vendor-b/fallback-model");
    assert_eq!(
        terminal.error_message.as_deref(),
        Some("fallback unavailable")
    );
    assert_eq!(terminal.fallback_models, Some(Vec::new()));
    assert_eq!(
        displays(terminal.fallback_attempts.as_ref()),
        vec!["vendor-a/primary-model", "vendor-b/fallback-model"]
    );
    assert_eq!(harness.store.list().expect("list").records.len(), 1);
}

#[test]
fn given_native_fallback_advanced_once_when_model_terminates_then_handoff_uses_remaining_rung() {
    let runner = FakeRunner::new();
    let harness = harness_with(
        fallback_plan(vec![
            category_record("vendor-b", "fallback-one"),
            category_record("vendor-c", "fallback-two"),
        ]),
        &runner,
        ExecutionMode::InProcess,
    );
    let task_id = start_in(&harness, ExecutionMode::InProcess);
    let handle = launched(&runner, &task_id, 1);
    handle.emit(&fallback_event(
        "vendor-a/primary-model",
        "vendor-b/fallback-one",
    ));
    let advanced = load(&harness, &task_id);
    assert_eq!(advanced.model, "vendor-b/fallback-one");
    assert_eq!(
        displays(advanced.fallback_models.as_ref()),
        vec!["vendor-c/fallback-two"]
    );
    assert_eq!(
        displays(advanced.fallback_attempts.as_ref()),
        vec!["vendor-a/primary-model", "vendor-b/fallback-one"]
    );

    handle.fail(
        RunnerFailureKind::ChildTurnFailed,
        "native fallback exhausted",
    );

    launched(&runner, &task_id, 2);
    assert_eq!(
        runner.specs()[1].model.as_deref(),
        Some("vendor-c/fallback-two")
    );
    let handed_off = load(&harness, &task_id);
    assert_eq!(handed_off.model, "vendor-c/fallback-two");
    assert_eq!(handed_off.fallback_models, Some(Vec::new()));
    assert_eq!(
        displays(handed_off.fallback_attempts.as_ref()),
        vec![
            "vendor-a/primary-model",
            "vendor-b/fallback-one",
            "vendor-c/fallback-two"
        ]
    );
}

#[test]
fn given_a_provisional_exit_when_runtime_fallback_hands_off_then_the_exit_is_dropped() {
    // fallback-handoff.ts handOffToNextRung: the failed rung's unconfirmed exit leaves with it.
    let runner = FakeRunner::new();
    let harness = harness_with(
        fallback_plan(vec![category_record("vendor-b", "fallback-model")]),
        &runner,
        ExecutionMode::Process,
    );
    let task_id = start_in(&harness, ExecutionMode::Process);
    let first = launched(&runner, &task_id, 1);
    let epoch = load(&harness, &task_id).notification.run_epoch;
    harness
        .store
        .mutate(&task_id, |record| TaskRecord {
            provisional_exit: Some(ProvisionalExit {
                observed_at: "2030-01-01T00:00:00.000Z".to_string(),
                run_epoch: epoch,
                code: Some(1),
                signal: None,
            }),
            ..record.clone()
        })
        .expect("seed provisional exit");

    first.fail(RunnerFailureKind::ChildPromptFailed, "primary unavailable");

    launched(&runner, &task_id, 2);
    let handed_off = load(&harness, &task_id);
    assert_eq!(handed_off.model, "vendor-b/fallback-model");
    assert_eq!(handed_off.notification.run_epoch, epoch + 1);
    assert_eq!(handed_off.provisional_exit, None);
}

// manager-runtime-fallback.test.ts

#[test]
fn given_running_category_child_when_host_applies_fallback_then_record_exposes_actual_model() {
    let runner = FakeRunner::new();
    let kimi = ResolvedModelRecord {
        reasoning_effort: Some("minimal".to_string()),
        ..category_record("kimi-coding", "kimi-for-coding-highspeed-unlocked")
    };
    let harness = harness_with(
        ResolvedChildPlan {
            model: "kimi-coding/kimi-for-coding-highspeed-unlocked".to_string(),
            category: Some("quick".to_string()),
            resolved_model: Some(kimi),
            ..ResolvedChildPlan::default()
        },
        &runner,
        ExecutionMode::InProcess,
    );
    let task_id = start_in(&harness, ExecutionMode::InProcess);
    let handle = launched(&runner, &task_id, 1);

    handle.emit(&fallback_event(
        "kimi-coding/kimi-for-coding-highspeed-unlocked",
        "quotio-openai/gpt-5.6-luna-fast:minimal",
    ));

    let record = load(&harness, &task_id);
    assert_eq!(record.model, "quotio-openai/gpt-5.6-luna-fast");
    let resolved = record.resolved_model.expect("resolved model");
    assert_eq!(resolved.source, ResolvedModelSource::Category);
    assert_eq!(resolved.provider, "quotio-openai");
    assert_eq!(resolved.model_id, "gpt-5.6-luna-fast");
    assert_eq!(resolved.display, "quotio-openai/gpt-5.6-luna-fast");
    assert_eq!(resolved.reasoning_effort.as_deref(), Some("minimal"));
    handle.settle(RunnerOutcome::completed("done"));
    wait_terminal(&harness.manager, &task_id);
}

#[test]
fn given_builtin_category_child_on_chain_rung_when_fallback_to_next_rung_then_record_advances() {
    let runner = FakeRunner::new();
    let host = registry(vec![
        model("openai-codex", "gpt-5.6-luna-fast"),
        model("opencode-go", "minimax-m3"),
    ]);
    let resolution = resolve_category(
        "quick",
        &serde_json::json!({}),
        &host,
        &ResolveCategoryOptions::default(),
    )
    .expect("host call");
    let CategoryResolutionResult::Resolved { spec, .. } = resolution else {
        panic!("expected resolved category, got {resolution:?}");
    };
    let harness = harness_with(
        ResolvedChildPlan {
            model: format!("{}/{}", spec.provider, spec.model_id),
            category: Some("quick".to_string()),
            resolved_model: Some(ResolvedModelRecord {
                variant: spec.variant.clone(),
                ..category_record(&spec.provider, &spec.model_id)
            }),
            requested_model: spec.requested_model.clone(),
            fallback_models: spec.fallback_models.clone(),
            ..ResolvedChildPlan::default()
        },
        &runner,
        ExecutionMode::InProcess,
    );
    let task_id = start_in(&harness, ExecutionMode::InProcess);
    let handle = launched(&runner, &task_id, 1);
    let initial = load(&harness, &task_id);
    assert_eq!(initial.model, "openai-codex/gpt-5.6-luna-fast");
    let chain = initial.fallback_models.expect("fallback chain");
    assert_eq!(chain.len(), 1);
    assert_eq!(chain[0].source, ResolvedModelSource::Category);
    assert_eq!(chain[0].provider, "opencode-go");
    assert_eq!(chain[0].model_id, "minimax-m3");
    assert_eq!(chain[0].variant.as_deref(), Some("max"));

    handle.emit(&fallback_event(
        "openai-codex/gpt-5.6-luna-fast",
        "opencode-go/minimax-m3:max",
    ));

    let record = load(&harness, &task_id);
    assert_eq!(record.model, "opencode-go/minimax-m3");
    let resolved = record.resolved_model.clone().expect("resolved model");
    assert_eq!(resolved.provider, "opencode-go");
    assert_eq!(resolved.model_id, "minimax-m3");
    assert_eq!(resolved.reasoning_effort.as_deref(), Some("max"));
    assert_eq!(record.fallback_models, Some(Vec::new()));
    let attempts: Vec<String> = record
        .fallback_attempts
        .unwrap_or_default()
        .iter()
        .map(|attempt| format!("{}/{}", attempt.provider, attempt.model_id))
        .collect();
    assert_eq!(
        attempts,
        vec!["openai-codex/gpt-5.6-luna-fast", "opencode-go/minimax-m3"]
    );
    handle.settle(RunnerOutcome::completed("done"));
    wait_terminal(&harness.manager, &task_id);
}

#[test]
fn start_failure_matrix_does_not_spend_same_account_parent_on_auth_or_quota() {
    for message in ["401 unauthorized", "weekly limit exhausted"] {
        let runner = FakeRunner::new();
        *super::fakes::lock(&runner.hook) = Some(Arc::new(move |_, call| {
            (call == 1).then(|| {
                Err(crate::manager::types::ManagedRunnerError::Runner(
                    crate::runners::RunnerFailure::new(
                        RunnerFailureKind::ModelUnavailable,
                        message,
                    ),
                ))
            })
        }));
        let harness = harness_with(
            fallback_plan(vec![
                category_record("vendor-b", "fallback-model"),
                category_record("vendor-a", "parent"),
            ]),
            &runner,
            ExecutionMode::InProcess,
        );
        let task = started(harness.manager.start(&ManagerStartSpec {
            parent_model: Some("vendor-a/parent".into()),
            ..base_spec()
        }));
        let record = load(&harness, &task.task_id);
        assert_eq!(record.model, "vendor-b/fallback-model");
        assert!(!displays(record.fallback_models.as_ref()).contains(&"vendor-a/parent".to_owned()));
        assert_eq!(runner.started_count(), 2);
    }
}

// auth-failure-fallback.test.ts (#10806): a typed provider_credentials_missing prompt refusal walks
// the chain past every rung on the refusing provider and records not_signed_in on the parent rung.
#[test]
fn credentials_refusal_skips_refusing_provider_and_records_not_signed_in() {
    let runner = FakeRunner::new();
    *super::fakes::lock(&runner.hook) = Some(Arc::new(move |_, call| {
        (call == 1).then(|| {
            let refusal = crate::runners::rpc::protocol_client::RpcClientError::Command(
                crate::runners::rpc::errors::RpcCommandError::with_refusal(
                    "prompt",
                    "No API key found for vendor-a.",
                    &serde_json::json!({"errorCode": "provider_credentials_missing", "errorData": {"provider": "vendor-a"}}),
                ),
            );
            Err(crate::manager::types::ManagedRunnerError::Runner(
                crate::runners::RunnerFailure::new(
                    RunnerFailureKind::ChildPromptFailed,
                    refusal.to_string(),
                )
                .with_cause(crate::manager::start_failure_cause::classify_start_cause(&refusal)),
            ))
        })
    }));
    let harness = harness_with(
        fallback_plan(vec![
            category_record("vendor-a", "sibling"),
            category_record("vendor-c", "parent"),
        ]),
        &runner,
        ExecutionMode::InProcess,
    );
    let task = started(harness.manager.start(&ManagerStartSpec {
        parent_model: Some("vendor-c/parent".into()),
        ..base_spec()
    }));
    let record = load(&harness, &task.task_id);
    assert_eq!(record.model, "vendor-c/parent");
    assert_eq!(record.fallback_models, Some(Vec::new()));
    assert_eq!(
        record.parent_model_fallback.map(|note| note.reason),
        Some(crate::state::ParentModelFallbackReason::NotSignedIn)
    );
    assert_eq!(runner.started_count(), 2);
}
