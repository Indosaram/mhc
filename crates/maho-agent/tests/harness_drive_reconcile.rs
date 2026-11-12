//! Ports of senpi `drive-reconcile.test.ts` and `drive-retry-deferred.test.ts` (pin 903b8336)
//! cases that `harness_drive_generation.rs` does not already cover.
//!
//! The pinned fixtures construct a real runtime `Lane` over `InstrumentedStorage(MemoryStorage
//! { now: 100 })` (optionally gated), register the faux provider on the lane's `Models`, record
//! every emitted event, and install one operation through `lane.command`. The Rust fixture does
//! the same with the production `Lane`; ordering is driven by oneshot handshakes and
//! `GatingStorage`, and every wait is bounded.

use maho_agent::harness::agent_harness::{DriveOptions, DriveOutcome};
use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::events::{HarnessEventBus, event_to_value};
use maho_agent::harness::hooks::{HookName, HookResult};
use maho_agent::harness::runtime::drive::checkpoint::{run_checkpoint, start_run};
use maho_agent::harness::runtime::drive::deferred::{run_deferred, run_deferred_suspended};
use maho_agent::harness::runtime::drive::drive_operation;
use maho_agent::harness::runtime::drive::generation::run_generation;
use maho_agent::harness::runtime::drive::reconcile::reconcile_operation;
use maho_agent::harness::runtime::drive::retry::{DriveClock, with_drive_clock};
use maho_agent::harness::runtime::lane::{
    CommitDecision, Drive, Lane, LaneCommand, OperationMismatch, ProcedureResult, PromptInput,
};
use maho_agent::harness::runtime::restore::{decode_operation_state, restore_lane};
use maho_agent::harness::session::commit::insert_entry;
use maho_agent::harness::session::testing::{GatingStorage, InstrumentedStorage};
use maho_agent::harness::session::types::{
    InboxItem, InboxItemKind, NewEntry, Operation, OperationMeta, OperationState, Session,
    SessionMetadata, SessionReader, Storage, TerminalStatus, Write,
};
use maho_agent::harness::session::values::{
    append_list, branch_tip, lane_config, lane_state, operation_meta, operation_preparation_prefix,
    operation_state, operation_tool_args_prefix, operation_tool_memo_prefix,
    pending_assistant_frames, pending_entry, set_value,
};
use maho_agent::harness::session::{
    MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions,
};
use maho_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxDeferredOptions, FauxProviderHandle, FauxTokenSize,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_tool_call,
};
use maho_ai::types::{BoxFuture, DeferredOption, StopReason};
use serde_json::{Value as Json, json};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OPERATION_ID: &str = "01950000-0000-7000-8000-000000000001";
const BOUND: Duration = Duration::from_secs(10);
const EVENT_TYPES: [&str; 28] = [
    "run_start",
    "run_resume",
    "run_suspend",
    "operation_abort",
    "run_end",
    "fault",
    "handler_error",
    "turn_start",
    "turn_end",
    "retry_scheduled",
    "retry_start",
    "retry_end",
    "message_start",
    "message_update",
    "message_end",
    "tool_start",
    "tool_update",
    "tool_end",
    "entry_added",
    "queue_update",
    "value_update",
    "config_update",
    "compaction_start",
    "compaction_end",
    "navigation_start",
    "navigation_end",
    "lane_created",
    "usage",
];

struct FixtureAuth;

impl maho_ai::models::ModelsAuth for FixtureAuth {
    fn resolve<'a>(
        &'a self,
        _: &'a dyn maho_ai::models::Provider,
        _: &'a maho_ai::models::AuthResolutionOverrides,
    ) -> BoxFuture<'a, Result<Option<maho_ai::models::AuthResolution>, maho_ai::models::ModelsError>>
    {
        Box::pin(async {
            Ok(Some(maho_ai::models::AuthResolution {
                ambient: false,
                auth: maho_ai::models::ProviderAuthResult {
                    api_key: Some("fixture".into()),
                    ..Default::default()
                },
                env: None,
            }))
        })
    }

    fn refresh_credential<'a>(
        &'a self,
        _: &'a dyn maho_ai::models::Provider,
        _: Option<&'a maho_ai::models::Credential>,
        _: &'a maho_ai::utils::abort::AbortSignal,
    ) -> BoxFuture<'a, Result<Option<maho_ai::models::Credential>, maho_ai::models::ModelsError>>
    {
        Box::pin(async { Ok(None) })
    }
}

struct Fixture {
    lane: Arc<Lane>,
    drive: Arc<Drive>,
    session: Arc<StorageBackedSession>,
    backend: Arc<MemoryStorage>,
    storage: Arc<InstrumentedStorage>,
    gating: Option<Arc<GatingStorage>>,
    faux: FauxProviderHandle,
    configuration: Json,
    events: Arc<Mutex<Vec<Json>>>,
    _subscriptions: Vec<maho_agent::harness::events::Unsubscribe>,
}

#[derive(Default)]
struct FixtureOptions {
    gated: bool,
    pending_fetches: u64,
    deferred_submission: bool,
}

impl Fixture {
    fn events(&self) -> Vec<Json> {
        self.events.lock().expect("events").clone()
    }

    fn event_types(&self) -> Vec<String> {
        self.events()
            .iter()
            .map(|event| event["type"].as_str().unwrap_or_default().to_owned())
            .collect()
    }

    fn current(&self) -> OperationState {
        self.lane
            .state()
            .operation
            .expect("fixture has no operation")
            .state
    }

    fn current_json(&self) -> Json {
        serde_json::to_value(self.current()).expect("state json")
    }

    fn install_drive(
        &self,
        wait_for_retry: Option<bool>,
        poll_deferred: Option<bool>,
    ) -> Arc<Drive> {
        let drive = Arc::new(Drive::new(
            &DriveOptions {
                operation_id: OPERATION_ID.into(),
                wait_for_retry,
                poll_deferred,
            },
            &BACKGROUND_CONTEXT,
        ));
        self.lane.set_active_drive(Some(drive.clone()));
        drive
    }

    fn on_hook(
        &self,
        name: HookName,
        handler: impl Fn(maho_agent::harness::hooks::HookInvocation) -> HookResult
        + Send
        + Sync
        + 'static,
    ) {
        // The registration lives as long as the registry; tests never unregister.
        let _ = self.lane.hooks.on(
            name,
            Arc::new(move |invocation, _| {
                let result = handler(invocation);
                Box::pin(async move { Ok(result) })
            }),
            None,
        );
    }

    async fn value(&self, address: &maho_agent::harness::session::values::Value) -> Option<Json> {
        self.session
            .get_value(address, &BACKGROUND_CONTEXT)
            .await
            .expect("value read")
            .map(|stored| stored.value)
    }

    async fn expect_projection_restores(&self) {
        assert_eq!(
            self.lane.state(),
            restore_lane(&*self.session, "main", &BACKGROUND_CONTEXT)
                .await
                .expect("restore lane")
        );
    }
}

fn user(content: &str) -> Json {
    json!({"role": "user", "content": content, "timestamp": 1})
}

fn scope(cancelled: bool) -> Json {
    json!({
        "control": if cancelled { json!({"status": "cancel_requested", "requestedAt": 10}) } else { json!({"status": "running"}) },
        "settings": {
            "compaction": maho_agent::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS,
            "steeringMode": "all",
            "followUpMode": "all",
            "toolExecution": "parallel"
        },
        "latestAssistantEntryId": null
    })
}

fn with(base: Json, fields: Json) -> Json {
    let mut merged = base;
    for (key, value) in fields.as_object().expect("fields") {
        merged[key] = value.clone();
    }
    merged
}

fn record(
    events: &Arc<Mutex<Vec<Json>>>,
    bus: &HarnessEventBus,
) -> Vec<maho_agent::harness::events::Unsubscribe> {
    EVENT_TYPES
        .iter()
        .map(|event_type| {
            let sink = events.clone();
            bus.on(
                event_type,
                Arc::new(move |event, _| {
                    sink.lock().expect("events").push(event_to_value(&event));
                    Box::pin(async {})
                }),
            )
        })
        .collect()
}

async fn create_fixture(options: FixtureOptions) -> Fixture {
    let backend = Arc::new(MemoryStorage::new(MemoryStorageOptions {
        now: Some(Arc::new(|| 100)),
    }));
    let gating = options
        .gated
        .then(|| Arc::new(GatingStorage::new(backend.clone())));
    let inner: Arc<dyn Storage> = match &gating {
        Some(gating) => gating.clone(),
        None => backend.clone(),
    };
    let storage = Arc::new(InstrumentedStorage::new(inner));
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata {
            id: "reconcile".into(),
            created_at: 1,
            storage_version: 1,
            cwd: None,
            parent_session_id: None,
            legacy_parent_session_path: None,
            path: None,
        },
        storage.clone(),
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    let faux = faux_provider(RegisterFauxProviderOptions {
        api: Some("faux".into()),
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
        }),
        deferred: Some(FauxDeferredOptions {
            pending_fetches: Some(options.pending_fetches),
            poll_after_ms: None,
        }),
        ..Default::default()
    })
    .expect("faux provider");
    let model = faux.get_model(None).expect("faux model");
    let configuration = json!({
        "model": {"provider": model.provider, "modelId": model.id},
        "thinkingLevel": "off",
        "activeToolNames": []
    });
    let mutation = session
        .begin_mutation(&BACKGROUND_CONTEXT)
        .await
        .expect("seed mutation");
    mutation
        .commit(
            vec![
                Write::Value(set_value(&branch_tip("main"), Json::Null)),
                Write::Value(set_value(&lane_config("main"), configuration.clone())),
                Write::Value(set_value(
                    &lane_state("main"),
                    json!({"currentOperationId": null, "lastOperationId": null, "inbox": []}),
                )),
            ],
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("seed lane");
    mutation.end(&BACKGROUND_CONTEXT).await;
    let restored = restore_lane(&*session, "main", &BACKGROUND_CONTEXT)
        .await
        .expect("restore lane");
    let mut lane = Lane::new(
        "main".into(),
        session.clone(),
        restored,
        HarnessEventBus::new(),
    );
    lane.models = maho_ai::models::create_models(Some(maho_ai::models::CreateModelsOptions {
        auth: Some(Arc::new(FixtureAuth)),
        ..Default::default()
    }));
    lane.models.set_provider(faux.provider.clone());
    lane.update_drive_config(|config| {
        config.retry_policy = maho_ai::utils::retry::RetryPolicy {
            enabled: true,
            max_retries: 3,
            base_delay_ms: 10,
            max_agent_delay_ms: None,
            random: None,
        };
        if options.deferred_submission {
            config.stream_options.deferred = Some(DeferredOption::Enabled(true));
        }
    });
    let lane = Arc::new(lane);
    lane.install_self();
    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriptions = record(&events, &lane.events);
    let drive = Arc::new(Drive::new(
        &DriveOptions {
            operation_id: OPERATION_ID.into(),
            wait_for_retry: None,
            poll_deferred: None,
        },
        &BACKGROUND_CONTEXT,
    ));
    lane.set_active_drive(Some(drive.clone()));
    storage.clear_commit_attempts();
    Fixture {
        lane,
        drive,
        session,
        backend,
        storage,
        gating,
        faux,
        configuration,
        events,
        _subscriptions: subscriptions,
    }
}

struct Installed {
    state: Json,
    intent: Json,
    entries: Option<Vec<Json>>,
    writes: Vec<Write>,
    terminal_events: Vec<&'static str>,
}

fn installed(state: Json, intent: Json, terminal_events: Vec<&'static str>) -> Installed {
    Installed {
        state,
        intent,
        entries: None,
        writes: Vec::new(),
        terminal_events,
    }
}

async fn install_operation(fixture: &Fixture, installed: &Installed) {
    let entries: Vec<NewEntry> = installed
        .entries
        .clone()
        .unwrap_or_else(|| {
            vec![json!({"id": "tip", "parentId": null, "type": "message", "message": user("history")})]
        })
        .into_iter()
        .map(|entry| serde_json::from_value(entry).expect("new entry"))
        .collect();
    let tip_id = entries.last().map(|entry| entry.id.clone());
    let meta: OperationMeta = serde_json::from_value(json!({
        "operationId": OPERATION_ID,
        "lane": "main",
        "sourceTipId": tip_id,
        "startedAt": 1,
        "intent": installed.intent
    }))
    .expect("operation meta");
    let state = decode_operation_state(installed.state.clone()).expect("fixture state");
    let extra = installed.writes.clone();
    fixture
        .lane
        .command(
            move |mut projection, _| {
                Box::pin(async move {
                    let mut writes: Vec<Write> = entries.into_iter().map(insert_entry).collect();
                    writes.push(Write::Value(set_value(&branch_tip("main"), json!(tip_id))));
                    writes.extend(extra);
                    writes.push(Write::Value(set_value(
                        &operation_meta(OPERATION_ID),
                        serde_json::to_value(&meta).expect("meta json"),
                    )));
                    writes.push(Write::Value(set_value(
                        &operation_state(OPERATION_ID),
                        serde_json::to_value(&state).expect("state json"),
                    )));
                    writes.push(Write::Value(set_value(
                        &lane_state("main"),
                        json!({"currentOperationId": OPERATION_ID, "lastOperationId": projection.last_operation_id, "inbox": projection.inbox}),
                    )));
                    projection.tip_id = tip_id;
                    projection.operation = Some(Operation { meta, state });
                    Ok(LaneCommand::Commit {
                        decision: CommitDecision {
                            writes,
                            materialize: Arc::new(|_| ()),
                            events: None,
                        },
                        next: Box::new(projection),
                    })
                })
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("install operation");
    fixture.storage.clear_commit_attempts();
}

/// Pinned `replaceRunState`: commit a new durable state for the open operation.
async fn replace_run_state(fixture: &Fixture, next: Json, extra: Vec<Write>) {
    let next = decode_operation_state(next).expect("next state");
    fixture
        .lane
        .command(
            move |mut projection, _| {
                Box::pin(async move {
                    let operation = projection
                        .operation
                        .as_mut()
                        .expect("fixture has no operation");
                    operation.state = next.clone();
                    let mut writes = extra;
                    writes.push(Write::Value(set_value(
                        &operation_state(OPERATION_ID),
                        serde_json::to_value(&next).expect("state json"),
                    )));
                    Ok(LaneCommand::Commit {
                        decision: CommitDecision {
                            writes,
                            materialize: Arc::new(|_| ()),
                            events: None,
                        },
                        next: Box::new(projection),
                    })
                })
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("replace run state");
}

fn generation(fixture: &Fixture) -> Json {
    json!({
        "stepId": "step",
        "triggerEntryId": "tip",
        "configuration": fixture.configuration,
        "streamOptions": {},
        "retryPolicy": {"maxAttempts": 2, "baseDelayMs": 10, "maxAgentDelayMs": 30_000},
        "overflowRecoveryUsed": false
    })
}

fn summary_context(fixture: &Fixture) -> Json {
    json!({
        "resultEntryId": "summary-entry",
        "configuration": fixture.configuration,
        "streamOptions": {},
        "retryPolicy": {"maxAttempts": 2, "baseDelayMs": 10, "maxAgentDelayMs": 30_000}
    })
}

/// Pinned `deferredOperation(fixture, effectPending)`.
fn deferred_operation(fixture: &Fixture, effect_pending: bool) -> (Json, Json) {
    let model = fixture.faux.get_model(None).expect("faux model");
    let handle = json!({"provider": model.provider, "modelId": model.id, "api": model.api, "id": "deferred-job"});
    let mut source = serde_json::to_value(faux_assistant_message(
        "",
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::Deferred),
            deferred: Some(serde_json::from_value(handle).expect("handle")),
            ..Default::default()
        },
    ))
    .expect("source json");
    source["content"] = json!([]);
    let entry =
        json!({"id": "deferred-source", "parentId": null, "type": "message", "message": source});
    let base = with(
        scope(true),
        json!({
            "stepId": "step",
            "sourceEntryId": "deferred-source",
            "poll": if effect_pending { 1 } else { 0 },
            "configuration": fixture.configuration,
            "streamOptions": {}
        }),
    );
    let state = if effect_pending {
        with(
            base,
            json!({"at": "deferred.effect_pending", "responseEntryId": "deferred-response", "usageId": "deferred-usage"}),
        )
    } else {
        with(base, json!({"at": "deferred.suspended"}))
    };
    (state, entry)
}

/// Pinned `cases(fixture)`: one cancelled operation per durable dispatcher leaf (13 leaves).
fn cases(fixture: &Fixture) -> Vec<Installed> {
    let run_task = json!({
        "taskId": "run-summary",
        "reason": "threshold",
        "boundary": {"kind": "resume_checkpoint", "resumeAfter": {"continuation": {"kind": "need_assistant", "overflowRecoveryUsed": false}, "triggerEntryId": "tip"}}
    });
    let compaction_task =
        json!({"taskId": "compaction", "reason": "manual", "boundary": {"kind": "finish"}});
    let navigation_task = json!({"taskId": "navigation", "boundary": {"kind": "commit_navigation", "targetId": "target"}});
    let run = json!({"kind": "run", "promptEntryIds": ["tip"]});
    // Pinned `Date.now() + 100_000`: far enough that no reconciliation path waits it out.
    let far = i64::MAX / 4;
    let (suspended, suspended_entry) = deferred_operation(fixture, false);
    let (deferred_effect, deferred_effect_entry) = deferred_operation(fixture, true);
    let mut tool_message = serde_json::to_value(faux_assistant_message(
        faux_tool_call("tool", Default::default(), None),
        FauxAssistantMessageOptions {
            stop_reason: Some(StopReason::ToolUse),
            ..Default::default()
        },
    ))
    .expect("tool message");
    tool_message["timestamp"] = json!(1);
    vec![
        installed(
            with(scope(true), json!({"at": "starting"})),
            run.clone(),
            vec!["run_end"],
        ),
        installed(
            with(
                scope(true),
                json!({"at": "checkpoint", "continuation": {"kind": "need_assistant", "overflowRecoveryUsed": false}, "triggerEntryId": "tip"}),
            ),
            run.clone(),
            vec!["run_end"],
        ),
        installed(
            with(
                scope(true),
                json!({"at": "assistant.ready", "generationContext": generation(fixture), "nextAttempt": 1}),
            ),
            run.clone(),
            vec!["run_end"],
        ),
        Installed {
            writes: vec![Write::List(append_list(
                &pending_assistant_frames(OPERATION_ID, "assistant-response"),
                json!({"type": "text_delta", "contentIndex": 0, "delta": "partial"}),
            ))],
            ..installed(
                with(
                    scope(true),
                    json!({
                        "at": "assistant.effect_pending",
                        "generationContext": generation(fixture),
                        "attempt": 1,
                        "responseEntryId": "assistant-response",
                        "usageId": "assistant-usage",
                        "intendedOutputLimit": 100,
                        "contextWindow": 1_000
                    }),
                ),
                run.clone(),
                vec!["run_end"],
            )
        },
        installed(
            with(
                scope(true),
                json!({"at": "assistant.retry_wait", "generationContext": generation(fixture), "nextAttempt": 2, "notBefore": far, "errorMessage": "retry"}),
            ),
            run.clone(),
            vec!["run_end"],
        ),
        Installed {
            entries: Some(vec![
                json!({"id": "assistant", "parentId": null, "type": "message", "message": tool_message}),
            ]),
            ..installed(
                with(
                    scope(true),
                    json!({
                        "at": "tools",
                        "batch": {
                            "assistantEntryId": "assistant",
                            "configuration": fixture.configuration,
                            "turnId": "turn",
                            "calls": [{"status": "planned", "sourceIndex": 0, "resultEntryId": "tool-result"}]
                        }
                    }),
                ),
                json!({"kind": "run", "promptEntryIds": []}),
                vec!["run_end"],
            )
        },
        Installed {
            entries: Some(vec![suspended_entry]),
            ..installed(
                suspended,
                json!({"kind": "run", "promptEntryIds": []}),
                vec!["run_end"],
            )
        },
        Installed {
            entries: Some(vec![deferred_effect_entry]),
            ..installed(
                deferred_effect,
                json!({"kind": "run", "promptEntryIds": []}),
                vec!["run_end"],
            )
        },
        installed(
            with(
                scope(true),
                json!({"at": "summary.deciding", "task": run_task}),
            ),
            run.clone(),
            vec!["compaction_end", "run_end"],
        ),
        installed(
            with(
                scope(true),
                json!({"at": "summary.ready", "task": compaction_task, "summaryContext": summary_context(fixture), "nextAttempt": 1}),
            ),
            json!({"kind": "compaction"}),
            vec!["compaction_end"],
        ),
        installed(
            with(
                scope(true),
                json!({
                    "at": "summary.effect_pending",
                    "task": navigation_task,
                    "summaryContext": summary_context(fixture),
                    "attempt": 1,
                    "request": {"index": 0, "usageId": "usage"},
                    "usageIds": []
                }),
            ),
            json!({"kind": "navigation", "targetId": "target", "summarize": true}),
            vec!["navigation_end"],
        ),
        installed(
            with(
                scope(true),
                json!({"at": "summary.retry_wait", "task": run_task, "summaryContext": summary_context(fixture), "nextAttempt": 2, "notBefore": far, "errorMessage": "retry"}),
            ),
            run,
            vec!["compaction_end", "run_end"],
        ),
        installed(
            with(
                scope(true),
                json!({"at": "navigation.ready_to_commit", "targetId": "target"}),
            ),
            json!({"kind": "navigation", "targetId": "target", "summarize": false}),
            vec!["navigation_end"],
        ),
    ]
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("procedure deadline")
}

fn settled(outcome: &DriveOutcome) -> Json {
    let DriveOutcome::Settled { outcome } = outcome else {
        panic!("expected settlement, got {outcome:?}")
    };
    serde_json::to_value(outcome).expect("outcome json")
}

fn count_hook(fixture: &Fixture, name: HookName) -> Arc<std::sync::atomic::AtomicUsize> {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    fixture.on_hook(name, move |_| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        HookResult::None
    });
    calls
}

// --- drive-reconcile.test.ts -----------------------------------------------------------------

#[tokio::test]
async fn drives_an_ordinary_run_through_all_direct_procedures_with_one_before_drive_hook() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    install_operation(
        &fixture,
        &installed(
            with(scope(false), json!({"at": "starting"})),
            json!({"kind": "run", "promptEntryIds": ["tip"]}),
            vec!["run_end"],
        ),
    )
    .await;
    let before_drive = count_hook(&fixture, HookName::BeforeDrive);
    fixture.faux.set_responses(vec![
        faux_assistant_message("answer", Default::default()).into(),
    ]);

    let outcome = settled(
        &bounded(drive_operation(fixture.lane.as_ref(), &fixture.drive))
            .await
            .expect("drive"),
    );
    assert_eq!(outcome["operationId"], OPERATION_ID);
    assert_eq!(outcome["kind"], "run");
    assert_eq!(outcome["status"], "completed");
    assert_eq!(before_drive.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(fixture.faux.state().call_count, 1);
    assert!(fixture.lane.state().operation.is_none());
}

#[tokio::test]
async fn reconciles_every_durable_leaf_without_ordinary_hook_admission() {
    for index in 0..13 {
        let fixture = create_fixture(FixtureOptions::default()).await;
        let case = cases(&fixture).remove(index);
        install_operation(&fixture, &case).await;
        let before_drive = count_hook(&fixture, HookName::BeforeDrive);

        let outcome = settled(
            &bounded(drive_operation(fixture.lane.as_ref(), &fixture.drive))
                .await
                .unwrap_or_else(|error| panic!("case {index}: {error:?}")),
        );
        assert_eq!(outcome["operationId"], OPERATION_ID, "case {index}");
        assert_eq!(outcome["status"], "aborted", "case {index}");
        assert_eq!(
            before_drive.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "case {index}"
        );
        assert!(fixture.lane.state().operation.is_none(), "case {index}");
        assert_eq!(
            fixture
                .lane
                .get_result(OPERATION_ID, &BACKGROUND_CONTEXT)
                .await
                .expect("result")
                .map(|record| record.status),
            Some(TerminalStatus::Aborted),
            "case {index}"
        );
        assert!(
            fixture.value(&operation_meta(OPERATION_ID)).await.is_none(),
            "case {index}"
        );
        assert!(
            fixture
                .value(&operation_state(OPERATION_ID))
                .await
                .is_none(),
            "case {index}"
        );
        for prefix in [
            operation_tool_args_prefix(OPERATION_ID, None),
            operation_tool_memo_prefix(OPERATION_ID, None),
            operation_preparation_prefix(OPERATION_ID),
        ] {
            assert!(
                fixture
                    .session
                    .scan_values(&prefix, &BACKGROUND_CONTEXT)
                    .await
                    .expect("scan")
                    .is_empty(),
                "case {index}: {prefix:?}"
            );
        }
        let at = case.state["at"].as_str().expect("at");
        if at == "deferred.suspended" || at == "deferred.effect_pending" {
            assert_eq!(
                fixture.faux.state().cancelled_deferred.len(),
                1,
                "case {index}"
            );
        }
        let terminal: Vec<String> = fixture
            .event_types()
            .into_iter()
            .filter(|kind| case.terminal_events.contains(&kind.as_str()))
            .collect();
        let tail = &terminal[terminal.len().saturating_sub(case.terminal_events.len())..];
        assert_eq!(tail, case.terminal_events.as_slice(), "case {index}");
    }
}

#[tokio::test]
async fn durably_drains_abortable_input_once_and_preserves_lane_owned_input_through_terminal_cleanup()
 {
    let fixture = create_fixture(FixtureOptions::default()).await;
    install_operation(
        &fixture,
        &installed(
            with(
                scope(false),
                json!({"at": "checkpoint", "continuation": {"kind": "need_assistant", "overflowRecoveryUsed": false}, "triggerEntryId": "tip"}),
            ),
            json!({"kind": "run", "promptEntryIds": ["tip"]}),
            vec!["run_end"],
        ),
    )
    .await;
    let queued = [
        ("steer", InboxItemKind::Steer, "steer"),
        ("write", InboxItemKind::Write, "write"),
        ("follow", InboxItemKind::FollowUp, "follow"),
        ("next", InboxItemKind::NextRun, "next"),
    ];
    let inbox: Vec<InboxItem> = queued
        .iter()
        .map(|(id, kind, _)| InboxItem {
            entry_id: (*id).into(),
            kind: *kind,
        })
        .collect();
    let pending: Vec<(String, Json)> = queued
        .iter()
        .map(|(id, _, content)| {
            (
                (*id).to_owned(),
                json!({"type": "message", "payload": user(content)}),
            )
        })
        .collect();
    let next_inbox = inbox.clone();
    fixture
        .lane
        .command(
            move |mut state, _| {
                Box::pin(async move {
                    let mut writes: Vec<Write> = pending
                        .into_iter()
                        .map(|(id, value)| Write::Value(set_value(&pending_entry(&id), value)))
                        .collect();
                    writes.push(Write::Value(set_value(
                        &lane_state("main"),
                        json!({"currentOperationId": OPERATION_ID, "lastOperationId": null, "inbox": next_inbox}),
                    )));
                    state.inbox = next_inbox;
                    Ok(LaneCommand::Commit {
                        decision: CommitDecision {
                            writes,
                            materialize: Arc::new(|_| ()),
                            events: None,
                        },
                        next: Box::new(state),
                    })
                })
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("queue input");
    fixture.storage.clear_commit_attempts();

    let requested = fixture
        .lane
        .request_operation_abort(OPERATION_ID.into(), &BACKGROUND_CONTEXT)
        .await
        .expect("no fault")
        .expect("abort requested");
    assert_eq!(requested.operation_id, OPERATION_ID);
    assert!(requested.newly_requested);
    assert_eq!(
        serde_json::to_value(&requested.steer).expect("steer")[0]["content"],
        "steer"
    );
    assert_eq!(
        serde_json::to_value(&requested.follow_up).expect("follow")[0]["content"],
        "follow"
    );
    assert!(fixture.drive.gate.signal().aborted());
    assert!(!fixture.drive.close_signal.aborted());
    let preserved = vec![
        InboxItem {
            entry_id: "write".into(),
            kind: InboxItemKind::Write,
        },
        InboxItem {
            entry_id: "next".into(),
            kind: InboxItemKind::NextRun,
        },
    ];
    assert_eq!(fixture.lane.state().inbox, preserved);
    assert!(fixture.value(&pending_entry("steer")).await.is_none());
    assert!(fixture.value(&pending_entry("follow")).await.is_none());
    let aborts: Vec<Json> = fixture
        .events()
        .into_iter()
        .filter(|event| event["type"] == "operation_abort")
        .collect();
    assert_eq!(aborts.len(), 1);
    assert_eq!(aborts[0]["operationId"], OPERATION_ID);
    assert_eq!(aborts[0]["steer"][0]["content"], "steer");
    assert_eq!(aborts[0]["followUp"][0]["content"], "follow");
    let commit_count = fixture.storage.get_commit_attempts().len();
    let event_count = fixture.events().len();
    let again = fixture
        .lane
        .request_operation_abort(OPERATION_ID.into(), &BACKGROUND_CONTEXT)
        .await
        .expect("no fault")
        .expect("abort repeated");
    assert!(!again.newly_requested);
    assert!(again.steer.is_empty());
    assert!(again.follow_up.is_empty());
    assert_eq!(fixture.storage.get_commit_attempts().len(), commit_count);
    assert_eq!(fixture.events().len(), event_count);

    let outcome = settled(
        &bounded(drive_operation(fixture.lane.as_ref(), &fixture.drive))
            .await
            .expect("drive"),
    );
    assert_eq!(outcome["status"], "aborted");
    assert_eq!(fixture.lane.state().inbox, preserved);
    assert!(fixture.value(&pending_entry("write")).await.is_some());
    assert!(fixture.value(&pending_entry("next")).await.is_some());
    fixture.storage.clear_commit_attempts();
    let stale: Result<_, OperationMismatch> = fixture
        .lane
        .request_operation_abort(OPERATION_ID.into(), &BACKGROUND_CONTEXT)
        .await
        .expect("no fault");
    assert!(stale.is_err());
    assert!(fixture.storage.get_commit_attempts().is_empty());
}

#[tokio::test]
async fn marks_cancellation_without_installing_a_drive() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    install_operation(
        &fixture,
        &installed(
            with(scope(false), json!({"at": "starting"})),
            json!({"kind": "run", "promptEntryIds": ["tip"]}),
            vec!["run_end"],
        ),
    )
    .await;
    fixture.lane.set_active_drive(None);

    let requested = fixture
        .lane
        .request_operation_abort(OPERATION_ID.into(), &BACKGROUND_CONTEXT)
        .await
        .expect("no fault")
        .expect("abort requested");
    assert!(requested.newly_requested);
    assert!(fixture.lane.active_drive_operation_id().is_none());
    assert_eq!(
        fixture.current_json()["control"]["status"],
        "cancel_requested"
    );
}

#[tokio::test]
async fn cancels_an_admitted_retry_timer_only_after_the_abort_marker_commits() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    // A deadline far in the future: the drive can only leave the timer through the abort.
    install_operation(
        &fixture,
        &installed(
            with(
                scope(false),
                json!({"at": "assistant.retry_wait", "generationContext": generation(&fixture), "nextAttempt": 2, "notBefore": i64::MAX / 4, "errorMessage": "retry"}),
            ),
            json!({"kind": "run", "promptEntryIds": ["tip"]}),
            vec!["run_end"],
        ),
    )
    .await;
    let drive = fixture.install_drive(Some(true), None);
    // Pinned `vi.waitFor(setTimeoutSpy)`: the retry wait reads the drive clock once to compare
    // the deadline and once more in `wait_until`, immediately before it parks on the timer. The
    // drive task cannot yield between that second read and the park, so once this task observes
    // the second read the drive is waiting on the timer.
    let (reads_tx, mut reads_rx) = tokio::sync::watch::channel(0usize);
    let clock: DriveClock = Arc::new(move || {
        reads_tx.send_modify(|reads| *reads += 1);
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_millis() as i64)
    });
    let lane = fixture.lane.clone();
    let running_drive = drive.clone();
    let running = tokio::spawn(with_drive_clock(clock, async move {
        drive_operation(lane.as_ref(), running_drive.as_ref()).await
    }));
    bounded(reads_rx.wait_for(|reads| *reads >= 2))
        .await
        .expect("drive parked on the retry timer");
    let requested = fixture
        .lane
        .request_operation_abort(OPERATION_ID.into(), &BACKGROUND_CONTEXT)
        .await
        .expect("no fault")
        .expect("abort requested");
    assert!(requested.newly_requested);
    let outcome = settled(&bounded(running).await.expect("drive task").expect("drive"));
    assert_eq!(outcome["status"], "aborted");
    assert!(!fixture.event_types().contains(&"retry_start".to_owned()));
}

#[tokio::test]
async fn drops_a_stale_structural_hook_result_when_cancellation_commits_first() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let task = json!({"taskId": "task", "reason": "manual", "boundary": {"kind": "finish"}});
    let deciding = with(
        scope(false),
        json!({"at": "summary.deciding", "task": task}),
    );
    install_operation(
        &fixture,
        &Installed {
            writes: vec![Write::Value(set_value(
                &maho_agent::harness::session::values::operation_preparation(OPERATION_ID, "task"),
                json!({
                    "kind": "compaction",
                    "messagesToSummarize": [user("history")],
                    "turnPrefixMessages": [],
                    "retainedTail": [],
                    "isSplitTurn": false,
                    "tokensBefore": 100,
                    "fileOps": {"read": [], "written": [], "edited": []},
                    "settings": maho_agent::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS
                }),
            ))],
            ..installed(deciding, json!({"kind": "compaction"}), vec!["compaction_end"])
        },
    )
    .await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let _ = fixture.lane.hooks.on(
        HookName::BeforeCompaction,
        Arc::new(move |_, _| {
            let started = started_tx.lock().expect("started").take();
            let release = release_rx.clone();
            Box::pin(async move {
                if let Some(started) = started {
                    let _ = started.send(());
                }
                if let Some(release) = release.lock().await.take() {
                    let _ = release.await;
                }
                Ok(HookResult::Structural {
                    decline: None,
                    value: Some(
                        json!({"summary": "stale", "tokensBefore": 100, "retainedTail": []}),
                    ),
                })
            })
        }),
        None,
    );
    let lane = fixture.lane.clone();
    let drive = fixture.drive.clone();
    let state = fixture.current();
    let running = tokio::spawn(async move {
        maho_agent::harness::runtime::types::RuntimeDriveLane::run_structural(
            lane.as_ref(),
            &drive,
            state,
        )
        .await
    });
    bounded(started_rx).await.expect("hook started");
    fixture
        .lane
        .request_operation_abort(OPERATION_ID.into(), &BACKGROUND_CONTEXT)
        .await
        .expect("no fault")
        .expect("abort requested");
    release_tx.send(()).expect("release hook");

    assert_eq!(
        bounded(running)
            .await
            .expect("decision task")
            .expect("decision"),
        ProcedureResult::Continue
    );
    let compactions = fixture
        .session
        .find_entries(
            Some(maho_agent::harness::session::types::EntryQuery {
                entry_type: Some(maho_agent::harness::session::types::EntryType::Compaction),
                ..Default::default()
            }),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("find entries");
    assert!(compactions.is_empty());
    let outcome = settled(
        &bounded(drive_operation(fixture.lane.as_ref(), &fixture.drive))
            .await
            .expect("drive"),
    );
    assert_eq!(outcome["status"], "aborted");
    let last = fixture.events().last().cloned().expect("last event");
    assert_eq!(last["type"], "compaction_end");
    assert_eq!(last["status"], "aborted");
}

#[tokio::test]
async fn can_crash_between_cancelled_response_settlement_and_terminal_cleanup() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    let case = cases(&fixture).remove(3);
    install_operation(&fixture, &case).await;

    assert_eq!(
        bounded(reconcile_operation(fixture.lane.as_ref(), &fixture.drive))
            .await
            .expect("reconcile"),
        ProcedureResult::Continue
    );
    assert_eq!(fixture.current_json()["at"], "checkpoint");
    let response = fixture
        .session
        .get_entry("assistant-response", &BACKGROUND_CONTEXT)
        .await
        .expect("entry")
        .expect("response entry");
    let response = serde_json::to_value(response).expect("entry json");
    assert_eq!(response["type"], "message");
    assert_eq!(response["message"]["stopReason"], "aborted");
    assert!(
        fixture
            .lane
            .get_result(OPERATION_ID, &BACKGROUND_CONTEXT)
            .await
            .expect("result")
            .is_none()
    );
    assert!(
        fixture
            .session
            .read_list(
                &pending_assistant_frames(OPERATION_ID, "assistant-response"),
                None,
                &BACKGROUND_CONTEXT,
            )
            .await
            .expect("frames")
            .is_empty()
    );

    let resumed = fixture.install_drive(None, None);
    let outcome = settled(
        &bounded(drive_operation(fixture.lane.as_ref(), &resumed))
            .await
            .expect("resumed drive"),
    );
    assert_eq!(outcome["status"], "aborted");
}

// --- drive-retry-deferred.test.ts ------------------------------------------------------------

async fn advance_to_ready(fixture: &Fixture) -> OperationState {
    let starting = fixture.current();
    bounded(start_run(fixture.lane.as_ref(), &fixture.drive, &starting))
        .await
        .expect("start run");
    let checkpoint = fixture.current();
    assert_eq!(fixture.current_json()["at"], "checkpoint");
    bounded(run_checkpoint(
        fixture.lane.as_ref(),
        &fixture.drive,
        &checkpoint,
    ))
    .await
    .expect("checkpoint");
    let ready = fixture.current();
    assert_eq!(fixture.current_json()["at"], "assistant.ready");
    ready
}

async fn accept_prompt(fixture: &Fixture) {
    fixture
        .lane
        .accept_prompt(
            PromptInput::Text {
                text: "question".into(),
                images: vec![],
            },
            Some(OPERATION_ID.into()),
            maho_agent::harness::session::types::RunSettings {
                compaction:
                    maho_agent::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS,
                steering_mode: maho_agent::types::QueueMode::All,
                follow_up_mode: maho_agent::types::QueueMode::All,
                tool_execution: maho_agent::harness::session::types::ToolExecutionMode::Parallel,
            },
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("no fault")
        .expect("admitted");
    fixture.storage.clear_commit_attempts();
}

fn retry_wait_from(ready: &OperationState, not_before: i64) -> Json {
    let mut state = serde_json::to_value(ready).expect("ready json");
    state["at"] = json!("assistant.retry_wait");
    state["nextAttempt"] = json!(2);
    state["notBefore"] = json!(not_before);
    state["errorMessage"] = json!("retry");
    state
}

#[tokio::test]
async fn classifies_a_live_retryable_provider_error_into_durable_retry_wait() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    accept_prompt(&fixture).await;
    let ready = advance_to_ready(&fixture).await;
    fixture.faux.set_responses(vec![
        faux_assistant_message(
            "",
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("503 service unavailable".into()),
                timestamp: Some(10),
                ..Default::default()
            },
        )
        .into(),
    ]);

    assert_eq!(
        bounded(run_generation(
            fixture.lane.as_ref(),
            &fixture.drive,
            &ready
        ))
        .await
        .expect("generation"),
        ProcedureResult::Continue
    );
    let state = fixture.current_json();
    assert_eq!(state["at"], "assistant.retry_wait");
    assert_eq!(state["nextAttempt"], 2);
    assert_eq!(state["errorMessage"], "503 service unavailable");
    let last = fixture.events().last().cloned().expect("last event");
    assert_eq!(last["type"], "retry_scheduled");
    assert_eq!(last["attempt"], 2);
    fixture.expect_projection_restores().await;
}

#[tokio::test]
async fn commits_ready_at_the_deadline_and_emits_retry_lifecycle_around_the_next_attempt() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    accept_prompt(&fixture).await;
    let ready = advance_to_ready(&fixture).await;
    // The pinned fake clock sits exactly at `notBefore`; an elapsed deadline is the same branch.
    replace_run_state(&fixture, retry_wait_from(&ready, 0), Vec::new()).await;
    fixture.storage.clear_commit_attempts();

    let retry_wait = fixture.current();
    assert_eq!(
        bounded(run_generation(
            fixture.lane.as_ref(),
            &fixture.drive,
            &retry_wait
        ))
        .await
        .expect("retry wait"),
        ProcedureResult::Continue
    );
    let next = fixture.current();
    assert_eq!(fixture.current_json()["at"], "assistant.ready");
    let last = fixture.events().last().cloned().expect("last event");
    assert_eq!(last["type"], "retry_start");
    assert_eq!(last["attempt"], 2);
    fixture.faux.set_responses(vec![
        faux_assistant_message(
            "retried",
            FauxAssistantMessageOptions {
                timestamp: Some(30),
                ..Default::default()
            },
        )
        .into(),
    ]);
    bounded(run_generation(fixture.lane.as_ref(), &fixture.drive, &next))
        .await
        .expect("retried generation");
    assert!(fixture.events().iter().any(|event| {
        event["type"] == "retry_end" && event["attempt"] == 2 && event["success"] == true
    }));
    fixture.expect_projection_restores().await;
}

#[test]
fn admits_an_abort_aware_timer_only_for_local_waiting() {
    on_large_stack(admits_an_abort_aware_timer_only_for_local_waiting_body);
}

async fn admits_an_abort_aware_timer_only_for_local_waiting_body() {
    let fixture = create_fixture(FixtureOptions::default()).await;
    accept_prompt(&fixture).await;
    let ready = advance_to_ready(&fixture).await;
    replace_run_state(&fixture, retry_wait_from(&ready, 3_100), Vec::new()).await;
    let drive = fixture.install_drive(Some(true), None);
    fixture.storage.clear_commit_attempts();
    let retry_wait = fixture.current();

    // Pinned `vi.useFakeTimers(); vi.setSystemTime(3_000)`: the drive clock is 3_000 plus paused
    // tokio time, so `tokio::time::advance` moves both the clock and the retry timer. Each clock
    // read is counted; the second read is `wait_until` computing its delay immediately before it
    // parks on the timer, so it proves the drive is waiting.
    tokio::time::pause();
    let start = tokio::time::Instant::now();
    let (reads_tx, mut reads_rx) = tokio::sync::watch::channel(0usize);
    let clock: DriveClock = Arc::new(move || {
        reads_tx.send_modify(|reads| *reads += 1);
        3_000 + start.elapsed().as_millis() as i64
    });
    let waiting = with_drive_clock(
        clock,
        run_generation(fixture.lane.as_ref(), &drive, &retry_wait),
    );
    let storage = fixture.storage.clone();
    let timeline = async move {
        reads_rx
            .wait_for(|reads| *reads >= 2)
            .await
            .expect("drive clock read");
        assert!(storage.get_commit_attempts().is_empty());
        tokio::time::advance(Duration::from_millis(99)).await;
        assert!(storage.get_commit_attempts().is_empty());
        tokio::time::advance(Duration::from_millis(1)).await;
    };
    let (result, ()) = bounded(async { tokio::join!(waiting, timeline) }).await;
    assert_eq!(result.expect("local wait"), ProcedureResult::Continue);
    assert!(!fixture.storage.get_commit_attempts().is_empty());
    let state = fixture.current_json();
    assert_eq!(state["at"], "assistant.ready");
    assert_eq!(state["nextAttempt"], 2);
}

async fn submit_deferred(
    fixture: &Fixture,
    response: maho_ai::types::AssistantMessage,
) -> OperationState {
    fixture.faux.set_responses(vec![response.into()]);
    let ready = advance_to_ready(fixture).await;
    bounded(run_generation(
        fixture.lane.as_ref(),
        &fixture.drive,
        &ready,
    ))
    .await
    .expect("submit deferred");
    let suspended = fixture.current();
    assert_eq!(
        fixture.current_json()["at"],
        "deferred.suspended",
        "initial request did not suspend"
    );
    suspended
}

fn done_message() -> maho_ai::types::AssistantMessage {
    faux_assistant_message(
        "done",
        FauxAssistantMessageOptions {
            timestamp: Some(20),
            ..Default::default()
        },
    )
}

async fn deferred_fixture(options: FixtureOptions) -> Fixture {
    let fixture = create_fixture(FixtureOptions {
        deferred_submission: true,
        ..options
    })
    .await;
    accept_prompt(&fixture).await;
    fixture
}

#[test]
fn plans_ready_deferred_tool_calls_with_the_poll_turn_identity_and_follower_result_ids() {
    on_large_stack(
        plans_ready_deferred_tool_calls_with_the_poll_turn_identity_and_follower_result_ids_body,
    );
}

async fn plans_ready_deferred_tool_calls_with_the_poll_turn_identity_and_follower_result_ids_body()
{
    let fixture = deferred_fixture(FixtureOptions::default()).await;
    let mut arguments = serde_json::Map::new();
    arguments.insert("query".into(), json!("value"));
    let suspended = submit_deferred(
        &fixture,
        faux_assistant_message(
            faux_tool_call("lookup", arguments, None),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::ToolUse),
                timestamp: Some(20),
                ..Default::default()
            },
        ),
    )
    .await;
    let step_id = serde_json::to_value(&suspended).expect("suspended")["stepId"]
        .as_str()
        .expect("step id")
        .to_owned();
    let drive = fixture.install_drive(None, Some(true));

    assert_eq!(
        bounded(run_deferred(fixture.lane.as_ref(), &drive, &suspended))
            .await
            .expect("poll"),
        ProcedureResult::Continue
    );
    let run = fixture.current_json();
    assert_eq!(run["at"], "tools");
    assert_eq!(run["batch"]["turnId"], format!("{step_id}:poll:1"));
    assert_eq!(run["batch"]["calls"][0]["status"], "planned");
    assert_eq!(run["batch"]["calls"][0]["sourceIndex"], 0);
    let result_id = run["batch"]["calls"][0]["resultEntryId"]
        .as_str()
        .expect("result id");
    let assistant_id = run["latestAssistantEntryId"]
        .as_str()
        .expect("assistant id");
    assert_eq!(&result_id[..13], &assistant_id[..13]);
    let last = fixture.events().last().cloned().expect("last event");
    assert_eq!(last["type"], "usage");
    fixture.expect_projection_restores().await;
}

#[tokio::test]
async fn consumes_its_permit_only_after_the_fresh_intent_commit_lands() {
    let fixture = deferred_fixture(FixtureOptions {
        gated: true,
        ..Default::default()
    })
    .await;
    let suspended = submit_deferred(
        &fixture,
        faux_assistant_message(
            Vec::<maho_ai::types::ContentBlock>::new(),
            FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error),
                error_message: Some("failed".into()),
                timestamp: Some(20),
                ..Default::default()
            },
        ),
    )
    .await;
    let gating = fixture.gating.clone().expect("gated fixture");
    let drive = fixture.install_drive(None, Some(true));
    gating.arm();
    let lane = fixture.lane.clone();
    let polling_drive = drive.clone();
    let polling = tokio::spawn(async move {
        run_deferred(lane.as_ref(), polling_drive.as_ref(), &suspended).await
    });

    bounded(gating.wait_pending(1))
        .await
        .expect("intent parked");
    assert_eq!(*drive.deferred_permits.lock().expect("permits"), 1);
    assert_eq!(fixture.faux.state().deferred_fetch_count, 0);
    bounded(gating.next(1)).await.expect("release intent");
    // The next parked commit (the start frame) can only exist after the fetch began.
    bounded(gating.wait_pending(1))
        .await
        .expect("start frame parked");
    assert_eq!(*drive.deferred_permits.lock().expect("permits"), 0);
    assert_eq!(fixture.faux.state().deferred_fetch_count, 1);
    bounded(gating.next(2))
        .await
        .expect("release start frame and settlement");
    let result = bounded(polling).await.expect("poll task").expect("poll");
    let ProcedureResult::Settled { outcome } = result else {
        panic!("expected settlement, got {result:?}")
    };
    assert_eq!(outcome.operation_id, OPERATION_ID);
    assert_eq!(outcome.status, TerminalStatus::Failed);
    assert!(fixture.lane.state().operation.is_none());
}

#[tokio::test]
async fn leaves_suspended_state_unchanged_when_the_fresh_intent_is_discarded() {
    let fixture = deferred_fixture(FixtureOptions {
        gated: true,
        ..Default::default()
    })
    .await;
    let suspended = submit_deferred(&fixture, done_message()).await;
    let gating = fixture.gating.clone().expect("gated fixture");
    let drive = fixture.install_drive(None, Some(true));
    gating.arm();
    let lane = fixture.lane.clone();
    let polling_drive = drive.clone();
    let polling = tokio::spawn(async move {
        run_deferred(lane.as_ref(), polling_drive.as_ref(), &suspended).await
    });

    bounded(gating.wait_pending(1))
        .await
        .expect("intent parked");
    gating.discard();
    let error = bounded(polling)
        .await
        .expect("poll task")
        .expect_err("discarded intent rejects");
    assert!(error.message.contains("commit discarded"), "{error:?}");
    assert_eq!(*drive.deferred_permits.lock().expect("permits"), 1);
    assert_eq!(fixture.faux.state().deferred_fetch_count, 0);
    let durable = fixture
        .backend
        .get_value(&operation_state(OPERATION_ID), &BACKGROUND_CONTEXT)
        .await
        .expect("backend read")
        .expect("durable state")
        .value;
    assert_eq!(durable["at"], "deferred.suspended");
    assert_eq!(durable["poll"], 0);
}

#[test]
fn waits_without_a_permit_then_performs_at_most_one_pending_poll_from_captured_options() {
    on_large_stack(
        waits_without_a_permit_then_performs_at_most_one_pending_poll_from_captured_options_body,
    );
}

async fn waits_without_a_permit_then_performs_at_most_one_pending_poll_from_captured_options_body()
{
    let fixture = deferred_fixture(FixtureOptions {
        pending_fetches: 1,
        ..Default::default()
    })
    .await;
    let suspended = submit_deferred(&fixture, done_message()).await;
    fixture.storage.clear_commit_attempts();
    let OperationState::DeferredSuspended(suspended) = suspended else {
        panic!("expected a deferred.suspended state")
    };

    let waiting = bounded(run_deferred_suspended(
        fixture.lane.as_ref(),
        &fixture.drive,
        &suspended,
    ))
    .await
    .expect("no permit");
    let ProcedureResult::Waiting {
        outcome:
            DriveOutcome::WaitingDeferred {
                operation_id,
                deferred,
            },
    } = waiting
    else {
        panic!("expected a deferred wait, got {waiting:?}")
    };
    assert_eq!(operation_id, OPERATION_ID);
    assert!(!deferred.id.is_empty());
    assert_eq!(fixture.faux.state().deferred_fetch_count, 0);
    assert!(fixture.storage.get_commit_attempts().is_empty());

    let hook_options = Arc::new(Mutex::new(None));
    let captured = hook_options.clone();
    fixture.on_hook(HookName::BeforeRequest, move |event| {
        if event.step.as_deref() == Some("deferred") {
            *captured.lock().expect("hook options") = Some(event.stream_options.clone());
        }
        HookResult::None
    });
    let drive = fixture.install_drive(None, Some(true));

    assert_eq!(
        bounded(run_deferred(
            fixture.lane.as_ref(),
            &drive,
            &fixture.current()
        ))
        .await
        .expect("poll"),
        ProcedureResult::Continue
    );
    assert_eq!(*drive.deferred_permits.lock().expect("permits"), 0);
    assert_eq!(
        hook_options
            .lock()
            .expect("hook options")
            .as_ref()
            .expect("hook ran")
            .deferred,
        Some(DeferredOption::Enabled(false))
    );
    let state = fixture.current_json();
    assert_eq!(state["at"], "deferred.suspended");
    assert_eq!(state["poll"], 1);
    assert_eq!(fixture.faux.state().deferred_fetch_count, 1);
    let types = fixture.event_types();
    assert_eq!(&types[types.len() - 2..], ["turn_end", "run_suspend"]);

    let again = bounded(run_deferred(
        fixture.lane.as_ref(),
        &drive,
        &fixture.current(),
    ))
    .await
    .expect("no second poll");
    assert!(matches!(
        again,
        ProcedureResult::Waiting {
            outcome: DriveOutcome::WaitingDeferred { .. }
        }
    ));
    assert_eq!(fixture.faux.state().deferred_fetch_count, 1);
    fixture.expect_projection_restores().await;
}

/// The debug-build drive futures for the retry-wait paths nest deeper than the 2 MiB default test
/// thread stack; the shipped binary runs its entry thread on 16 MiB (maho-cli main.rs).
fn on_large_stack<F: std::future::Future<Output = ()> + 'static>(body: fn() -> F) {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime")
                .block_on(body())
        })
        .expect("spawn test thread")
        .join()
        .expect("test thread panicked");
}
