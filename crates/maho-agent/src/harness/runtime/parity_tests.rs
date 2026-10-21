use std::sync::Arc;
use std::time::Duration;

use super::lane::{Lane, PromptInput};
use super::types::{Drive, LaneState};
use crate::harness::agent_harness::{DriveOptions, DriveOptionsError};
use crate::harness::context::{BACKGROUND_CONTEXT, with_abort_signal};
use crate::harness::events::HarnessEventBus;
use crate::harness::execution::effect_gate::{Cancellation, GateRefusal, create_gate};
use crate::harness::session::memory::MemoryStorage;
use crate::harness::session::session::{
    SessionError, SessionErrorKind, StorageBackedSession, StorageBackedSessionOptions,
};
use crate::harness::session::types::{LaneConfiguration, LaneModelRef, SessionMetadata};
use maho_ai::utils::abort::{AbortController, AbortReason};

fn lane() -> Arc<Lane> {
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata {
            id: "runtime-parity".into(),
            created_at: 1,
            storage_version: 1,
            cwd: None,
            parent_session_id: None,
            legacy_parent_session_path: None,
        },
        Arc::new(MemoryStorage::new(Default::default())),
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    Arc::new(Lane::new(
        "main".into(),
        session,
        LaneState {
            tip_id: None,
            configuration: LaneConfiguration {
                model: LaneModelRef {
                    provider: "test".into(),
                    model_id: "model".into(),
                },
                thinking_level: maho_ai::types::ModelThinkingLevel::Off,
                active_tool_names: vec![],
            },
            inbox: vec![],
            last_operation_id: None,
            operation: None,
        },
        HarnessEventBus::new(),
    ))
}

#[tokio::test]
async fn lazy_lane_acquisition_restores_marker_selected_operation_state() {
    // Given an open harness and a durable lane acquired only after it opened.
    let source = lane();
    let harness = super::harness::create_agent_harness(
        source.session.clone(),
        source.state().configuration,
        &BACKGROUND_CONTEXT,
    )
    .await
    .expect("open empty harness");
    let state = serde_json::json!({
        "at":"navigation.ready_to_commit", "targetId":null, "control":{"status":"running"}, "latestAssistantEntryId":null,
        "settings":{"compaction":{"enabled":true,"reserveTokens":1000,"keepRecentTokens":2000},"steeringMode":"all","followUpMode":"all","toolExecution":"parallel"}
    });
    store_lazy_lane(
        &harness,
        "op",
        "late",
        serde_json::json!({
            "kind":"navigation", "targetId":null, "summarize":false
        }),
        state,
    )
    .await;

    // When the uncached existing lane is acquired.
    let restored = harness
        .lane("late", None, &BACKGROUND_CONTEXT)
        .await
        .expect("restore lane");

    // Then its operation is decoded by its durable marker, not untagged variant order.
    assert!(matches!(
        restored.state().operation.expect("active operation").state,
        crate::harness::session::types::OperationState::NavigationReadyToCommit(_)
    ));
}

#[tokio::test]
async fn lazy_lane_acquisition_rejects_invalid_operation_ownership_and_intent() {
    for (operation_id, owner, intent) in [
        ("wrong-id", "late", "run"),
        ("op", "other-lane", "run"),
        ("op", "late", "compaction"),
    ] {
        // Given inconsistent metadata written after harness creation.
        let source = lane();
        let harness = super::harness::create_agent_harness(
            source.session.clone(),
            source.state().configuration,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("open empty harness");
        let state = serde_json::json!({
            "at":"starting", "control":{"status":"running"}, "latestAssistantEntryId":null,
            "settings":{"compaction":{"enabled":true,"reserveTokens":1000,"keepRecentTokens":2000},"steeringMode":"all","followUpMode":"all","toolExecution":"parallel"}
        });
        store_lazy_lane(
            &harness,
            operation_id,
            owner,
            serde_json::json!({"kind":intent,"promptEntryIds":[]}),
            state,
        )
        .await;

        // When the uncached existing lane is acquired.
        let result = harness.lane("late", None, &BACKGROUND_CONTEXT).await;

        // Then invalid durable operations cannot be published as usable lanes.
        assert!(result.is_err());
        assert!(
            !harness
                .lanes_by_name
                .lock()
                .expect("lanes lock")
                .contains_key("late")
        );
    }
}

async fn store_lazy_lane(
    harness: &super::harness::Harness,
    operation_id: &str,
    owner: &str,
    intent: serde_json::Value,
    state: serde_json::Value,
) {
    use crate::harness::session::{types::Write, values::*};
    let mutation = harness
        .session
        .begin_mutation(&BACKGROUND_CONTEXT)
        .await
        .expect("mutation");
    let values = [
        (branch_tip("late"), serde_json::Value::Null),
        (
            lane_config("late"),
            serde_json::to_value(lane().state().configuration).expect("configuration"),
        ),
        (
            lane_state("late"),
            serde_json::json!({"currentOperationId":"op","lastOperationId":null,"inbox":[]}),
        ),
        (
            operation_meta("op"),
            serde_json::json!({
                "operationId":operation_id, "lane":owner, "sourceTipId":null, "startedAt":1,
                "intent":intent
            }),
        ),
        (operation_state("op"), state),
    ];
    mutation
        .commit(
            values
                .into_iter()
                .map(|(address, value)| Write::Value(set_value(&address, value)))
                .collect(),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("write late lane");
    mutation.end(&BACKGROUND_CONTEXT).await;
}

#[tokio::test]
async fn typed_abort_waits_for_durable_cancellation() {
    let (gate, control) = create_gate();
    let cancellation = Cancellation::new();
    control.begin_abort(cancellation.clone());
    let refusal = gate.admit(|| ());
    let wait = super::drive::admit(refusal);
    tokio::pin!(wait);
    assert!(futures::poll!(wait.as_mut()).is_pending());
    cancellation.resolve();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), wait)
            .await
            .expect("cancellation settles")
            .expect("typed abort"),
        None
    );
}

#[tokio::test]
async fn closed_admission_is_not_cancellation() {
    let error = super::drive::admit::<()>(Err(GateRefusal::Closed("closed".into())))
        .await
        .expect_err("closed gate rejects");
    assert_eq!(error.kind, SessionErrorKind::Invariant);
}

#[tokio::test]
async fn progress_and_watch_reject_a_closed_lane() {
    let lane = lane();
    lane.seal(SessionError::new(SessionErrorKind::Closed, "closed"));
    let drive = Drive::new(
        &DriveOptions {
            operation_id: "op".into(),
            wait_for_retry: None,
            poll_deferred: None,
        },
        &BACKGROUND_CONTEXT,
    );
    let progress = super::progress::open_tool_progress(lane.clone(), &drive, "turn", 0, "result");
    progress.write(crate::types::AgentToolResult::text("progress"));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), progress.drain())
            .await
            .expect("progress drains")
            .expect_err("closed progress rejects")
            .kind,
        SessionErrorKind::Closed
    );
    assert!(
        super::transcript::watch_lane(lane, HarnessEventBus::new(), &BACKGROUND_CONTEXT)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn observing_drive_completion_honors_caller_cancellation() {
    let lane = lane();
    let settings = crate::harness::session::types::RunSettings {
        compaction: crate::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS,
        steering_mode: crate::types::QueueMode::All,
        follow_up_mode: crate::types::QueueMode::All,
        tool_execution: crate::harness::session::types::ToolExecutionMode::Parallel,
    };
    let admission = lane
        .accept_prompt(
            PromptInput::Text {
                text: "hello".into(),
                images: vec![],
            },
            None,
            settings,
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("admit command")
        .expect("prompt accepted");
    let (entered, receive) = tokio::sync::oneshot::channel();
    let entered = Arc::new(std::sync::Mutex::new(Some(entered)));
    let release = Arc::new(tokio::sync::Notify::new());
    let hook_release = release.clone();
    let _hook = lane.hooks.on(
        crate::harness::hooks::HookName::BeforeDrive,
        Arc::new(move |_, _| {
            let entered = entered.clone();
            let release = hook_release.clone();
            Box::pin(async move {
                let ready = release.notified();
                tokio::pin!(ready);
                ready.as_mut().enable();
                entered
                    .lock()
                    .expect("hook signal lock")
                    .take()
                    .expect("one drive")
                    .send(())
                    .expect("test awaits hook");
                ready.await;
                Ok(crate::harness::hooks::HookResult::None)
            })
        }),
        None,
    );
    let options = DriveOptions {
        operation_id: admission.operation_id,
        wait_for_retry: None,
        poll_deferred: None,
    };
    let owner = {
        let lane = lane.clone();
        let options = options.clone();
        tokio::spawn(async move { lane.drive(options, &BACKGROUND_CONTEXT).await })
    };
    tokio::time::timeout(Duration::from_secs(1), receive)
        .await
        .expect("hook starts")
        .expect("hook entered");
    let controller = AbortController::new();
    let context = with_abort_signal(controller.signal(), &BACKGROUND_CONTEXT);
    let observe = lane.drive(options, &context);
    tokio::pin!(observe);
    assert!(futures::poll!(observe.as_mut()).is_pending());
    let reason = AbortReason::new("CallerCancelled", "observer cancellation").with_payload(42_u64);
    controller.abort(Some(reason.clone()));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), observe)
            .await
            .expect("observer cancels"),
        Err(DriveOptionsError::Cancelled(reason))
    );
    release.notify_waiters();
    tokio::time::timeout(Duration::from_secs(1), owner)
        .await
        .expect("owner finishes")
        .expect("owner task")
        .expect("owner was not cancelled");
}

#[tokio::test]
async fn drive_rejection_preserves_typed_caller_reason_without_faulting_harness() {
    // Given an open harness and an already-cancelled caller with a typed gate reason.
    let source = lane();
    let harness = super::harness::create_agent_harness(
        source.session.clone(),
        source.state().configuration,
        &BACKGROUND_CONTEXT,
    )
    .await
    .expect("open harness");
    let lane = harness
        .lane("main", None, &BACKGROUND_CONTEXT)
        .await
        .expect("lane");
    let (gate, control) = create_gate();
    let cancellation = Cancellation::new();
    control.begin_abort(cancellation.clone());
    control.signal_abort();
    let reason = gate.signal().reason().expect("typed gate reason");
    let context = with_abort_signal(gate.signal(), &BACKGROUND_CONTEXT);

    // When drive admission is attempted, cancellation wins over an operation mismatch.
    let result = lane
        .drive(
            DriveOptions {
                operation_id: "missing".into(),
                wait_for_retry: None,
                poll_deferred: None,
            },
            &context,
        )
        .await;

    // Then the original type and handle survive, and the harness remains usable.
    let Err(DriveOptionsError::Cancelled(actual)) = result else {
        panic!("drive must reject with typed cancellation");
    };
    assert_eq!(actual, reason);
    assert_eq!(
        actual
            .payload::<crate::harness::execution::effect_gate::AbortRequested>()
            .expect("original typed payload")
            .cancellation,
        cancellation,
    );
    assert!(harness.lanes().is_ok());
    assert!(matches!(
        lane.drive(
            DriveOptions {
                operation_id: "missing".into(),
                wait_for_retry: None,
                poll_deferred: None
            },
            &BACKGROUND_CONTEXT,
        )
        .await,
        Err(DriveOptionsError::Mismatch(_))
    ));
}

#[tokio::test]
async fn unrelated_procedure_failure_survives_an_aborting_gate() {
    let lane = lane();
    let scope: crate::harness::session::types::OperationState = serde_json::from_value(serde_json::json!({
        "at":"starting", "control":{"status":"running"}, "latestAssistantEntryId":null,
        "settings":{"compaction":{"enabled":true,"reserveTokens":1000,"keepRecentTokens":2000},"steeringMode":"all","followUpMode":"all","toolExecution":"parallel"}
    })).expect("starting state");
    let mut state = lane.state();
    state.operation = Some(crate::harness::session::types::Operation {
        meta: crate::harness::session::types::OperationMeta {
            operation_id: "op".into(),
            lane: "main".into(),
            source_tip_id: None,
            started_at: 1,
            intent: crate::harness::session::types::OperationIntent::Run {
                prompt_entry_ids: vec!["missing".into()],
            },
        },
        state: scope,
    });
    super::types::RuntimeLane::publish_state(lane.as_ref(), state);
    let drive = Drive::new(
        &DriveOptions {
            operation_id: "op".into(),
            wait_for_retry: None,
            poll_deferred: None,
        },
        &BACKGROUND_CONTEXT,
    );
    let cancellation = Cancellation::new();
    cancellation.resolve();
    drive.begin_abort(cancellation);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        super::drive::drive_operation(lane.as_ref(), &drive),
    )
    .await
    .expect("procedure rejects without retrying");
    assert_eq!(
        result.expect_err("missing prompt is not cancellation").kind,
        SessionErrorKind::Invariant
    );
}

#[tokio::test]
async fn bounded_read_faults_through_its_lane_owner() {
    let lane = lane();
    let drive = Drive::new(
        &DriveOptions {
            operation_id: "op".into(),
            wait_for_retry: None,
            poll_deferred: None,
        },
        &BACKGROUND_CONTEXT,
    );
    let capability = super::restore::decode_operation_state(serde_json::json!({
        "at":"starting", "control":{"status":"running"}, "latestAssistantEntryId":null,
        "settings":{"compaction":{"enabled":true,"reserveTokens":1000,"keepRecentTokens":2000},"steeringMode":"all","followUpMode":"all","toolExecution":"parallel"}
    })).expect("starting capability");
    assert!(
        super::transcript::read_bounded_entries(lane.as_ref(), &drive, &capability)
            .await
            .is_err()
    );
    assert!(lane.assert_open().is_err());
}
