//! Port of senpi `packages/agent/test/harness/runtime/drive-structural.test.ts` (pin 903b8336).
//!
//! The pinned suite drives the private structural procedures (`runStructuralDecision`,
//! `runStructuralGeneration`, `runStructuralRetryWait`, `recoverStructuralGeneration`) directly,
//! so the port lives beside them as an in-crate child module. The fixture mirrors the pinned
//! `createFixture`: an `InstrumentedStorage` over `MemoryStorage { now: 100 }`, a faux provider
//! registered on the lane's `Models`, a recording event listener, and one installed operation.

use super::*;
use crate::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS;
use crate::harness::context::BACKGROUND_CONTEXT;
use crate::harness::events::{HarnessEventBus, event_to_value};
use crate::harness::execution::effect_gate::Cancellation;
use crate::harness::hooks::HookInvocation;
use crate::harness::runtime::drive::checkpoint::run_checkpoint;
use crate::harness::runtime::drive::generation::run_generation;
use crate::harness::runtime::lane::{CommitDecision, LaneCommand};
use crate::harness::runtime::restore::{decode_operation_state, restore_lane};
use crate::harness::session::testing::InstrumentedStorage;
use crate::harness::session::types::{
    InboxItem, Operation, OperationMeta, Session, SessionMetadata, SessionMutation,
};
use crate::harness::session::values::{
    lane_config, lane_state, operation_meta, operation_state, pending_entry,
};
use crate::harness::session::{
    MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions,
};
use maho_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxProviderHandle, FauxResponseStep, FauxTokenSize,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider,
};
use serde_json::{Value as Json, json};
use std::time::Duration;

const OPERATION_ID: &str = "01950000-0000-7000-8000-000000000001";
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
const BOUND: Duration = Duration::from_secs(10);

/// Fixture auth (the pinned faux provider resolves empty request auth).
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
    drive: Drive,
    session: Arc<StorageBackedSession>,
    storage: Arc<InstrumentedStorage>,
    faux: FauxProviderHandle,
    events: Arc<Mutex<Vec<Json>>>,
    configuration: Json,
    _subscriptions: Vec<crate::harness::events::Unsubscribe>,
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

    fn count(&self, event_type: &str) -> usize {
        self.event_types()
            .iter()
            .filter(|kind| *kind == event_type)
            .count()
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

    fn commit_writes(&self) -> Vec<Json> {
        self.storage
            .get_commit_attempts()
            .into_iter()
            .flatten()
            .map(|write| serde_json::to_value(write).expect("write json"))
            .collect()
    }

    fn on_hook(
        &self,
        name: HookName,
        handler: impl Fn(HookInvocation) -> HookResult + Send + Sync + 'static,
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

    async fn entry(&self, id: &str) -> Option<Json> {
        self.session
            .get_entry(id, &BACKGROUND_CONTEXT)
            .await
            .expect("entry read")
            .map(|entry| serde_json::to_value(entry).expect("entry json"))
    }

    async fn value(&self, address: &crate::harness::session::values::Value) -> Option<Json> {
        self.session
            .get_value(address, &BACKGROUND_CONTEXT)
            .await
            .expect("value read")
            .map(|stored| stored.value)
    }
}

fn run_scope(compaction: Json) -> Json {
    json!({
        "control": {"status": "running"},
        "settings": {
            "compaction": compaction,
            "steeringMode": "all",
            "followUpMode": "all",
            "toolExecution": "parallel"
        },
        "latestAssistantEntryId": null
    })
}

fn default_scope() -> Json {
    run_scope(serde_json::to_value(DEFAULT_COMPACTION_SETTINGS).expect("settings"))
}

fn with_scope(scope: Json, fields: Json) -> Json {
    let mut merged = scope;
    for (key, value) in fields.as_object().expect("fields object") {
        merged[key] = value.clone();
    }
    merged
}

fn user(content: &str) -> Json {
    json!({"role": "user", "content": content, "timestamp": 1})
}

fn assistant(text: &str) -> Json {
    serde_json::to_value(faux_assistant_message(text, Default::default())).expect("assistant json")
}

fn failure(message: &str) -> maho_ai::types::AssistantMessage {
    faux_assistant_message(
        "",
        FauxAssistantMessageOptions {
            stop_reason: Some(maho_ai::types::StopReason::Error),
            error_message: Some(message.to_owned()),
            ..Default::default()
        },
    )
}

fn message_entry(id: &str, parent: Option<&str>, message: Json) -> Json {
    json!({"id": id, "parentId": parent, "type": "message", "message": message})
}

fn standalone_compaction_task() -> Json {
    json!({"taskId": "task", "reason": "manual", "boundary": {"kind": "finish"}})
}

fn run_compaction_task(reason: &str, resume_after: Json) -> Json {
    json!({"taskId": "task", "reason": reason, "boundary": {"kind": "resume_checkpoint", "resumeAfter": resume_after}})
}

fn navigation_summary_task(target_id: &str) -> Json {
    json!({"taskId": "task", "boundary": {"kind": "commit_navigation", "targetId": target_id}})
}

fn retry_policy(max_attempts: u32) -> Json {
    json!({"maxAttempts": max_attempts, "baseDelayMs": 10, "maxAgentDelayMs": 30_000})
}

fn summary_ready(scope: Json, task: Json, configuration: &Json, max_attempts: u32) -> Json {
    with_scope(
        scope,
        json!({
            "at": "summary.ready",
            "task": task,
            "summaryContext": {
                "resultEntryId": "summary-entry",
                "configuration": configuration,
                "streamOptions": {},
                "retryPolicy": retry_policy(max_attempts)
            },
            "nextAttempt": 1
        }),
    )
}

fn summary_deciding(scope: Json, task: Json) -> Json {
    with_scope(scope, json!({"at": "summary.deciding", "task": task}))
}

fn compaction_preparation(overrides: Json) -> Json {
    with_scope(
        json!({
            "kind": "compaction",
            "messagesToSummarize": [user("history")],
            "turnPrefixMessages": [],
            "retainedTail": [user("tail")],
            "isSplitTurn": false,
            "tokensBefore": 1_000,
            "fileOps": {"read": [], "written": [], "edited": []},
            "settings": {"enabled": true, "reserveTokens": 1_000, "keepRecentTokens": 10}
        }),
        overrides,
    )
}

fn branch_preparation() -> Json {
    json!({
        "kind": "branch_summary",
        "messages": [user("abandoned")],
        "fileOps": {"read": [], "written": [], "edited": []},
        "totalTokens": 10
    })
}

fn decode(state: &Json) -> OperationState {
    decode_operation_state(state.clone()).expect("fixture state decodes")
}

fn record(
    events: &Arc<Mutex<Vec<Json>>>,
    bus: &HarnessEventBus,
) -> Vec<crate::harness::events::Unsubscribe> {
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

async fn create_fixture() -> Fixture {
    let storage = Arc::new(InstrumentedStorage::new(Arc::new(MemoryStorage::new(
        MemoryStorageOptions {
            now: Some(Arc::new(|| 100)),
        },
    ))));
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata {
            id: "structural".into(),
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
        token_size: Some(FauxTokenSize {
            min: Some(1),
            max: Some(1),
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
    {
        let mut config = lane.drive_config.lock().expect("drive config");
        config.retry_policy = maho_ai::utils::retry::RetryPolicy {
            enabled: true,
            max_retries: 1,
            base_delay_ms: 10,
            max_agent_delay_ms: None,
            random: None,
        };
    }
    let lane = Arc::new(lane);
    lane.install_self();
    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriptions = record(&events, &lane.events);
    storage.clear_commit_attempts();
    Fixture {
        drive: Drive::new(
            &crate::harness::agent_harness::DriveOptions {
                operation_id: OPERATION_ID.into(),
                wait_for_retry: None,
                poll_deferred: None,
            },
            &BACKGROUND_CONTEXT,
        ),
        lane,
        session,
        storage,
        faux,
        events,
        configuration,
        _subscriptions: subscriptions,
    }
}

struct Install {
    entries: Vec<Json>,
    tip_id: Option<Option<String>>,
    preparation: Option<Json>,
}

impl Install {
    fn entries(entries: Vec<Json>) -> Self {
        Self {
            entries,
            tip_id: None,
            preparation: None,
        }
    }

    fn prepared(entries: Vec<Json>, preparation: Json) -> Self {
        Self {
            entries,
            tip_id: None,
            preparation: Some(preparation),
        }
    }

    fn tip(mut self, tip: &str) -> Self {
        self.tip_id = Some(Some(tip.to_owned()));
        self
    }
}

async fn install_operation(fixture: &Fixture, state: &Json, intent: Json, options: Install) {
    let tip_id = options.tip_id.unwrap_or_else(|| {
        options
            .entries
            .last()
            .map(|entry| entry["id"].as_str().expect("entry id").to_owned())
    });
    let meta: OperationMeta = serde_json::from_value(json!({
        "operationId": OPERATION_ID,
        "lane": "main",
        "sourceTipId": tip_id,
        "startedAt": 1,
        "intent": intent
    }))
    .expect("operation meta");
    let state = decode(state);
    let entries: Vec<NewEntry> = options
        .entries
        .iter()
        .map(|entry| serde_json::from_value(entry.clone()).expect("new entry"))
        .collect();
    let preparation = options.preparation;
    fixture
        .lane
        .command(
            move |mut projection, _| {
                Box::pin(async move {
                    let mut writes: Vec<Write> = entries.into_iter().map(insert_entry).collect();
                    writes.push(Write::Value(set_value(&branch_tip("main"), json!(tip_id))));
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
                        json!({"currentOperationId": OPERATION_ID, "lastOperationId": null, "inbox": projection.inbox}),
                    )));
                    if let Some(preparation) = preparation {
                        writes.push(Write::Value(set_value(
                            &operation_preparation(OPERATION_ID, "task"),
                            preparation,
                        )));
                    }
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

/// Pinned inline `lane.command` that queues pending entries and rewrites `pi.lane.state`.
async fn queue_input(fixture: &Fixture, items: Vec<(&str, InboxItemKind, Json)>) {
    let items: Vec<(String, InboxItemKind, Json)> = items
        .into_iter()
        .map(|(id, kind, pending)| (id.to_owned(), kind, pending))
        .collect();
    fixture
        .lane
        .command(
            move |mut projection, _| {
                Box::pin(async move {
                    let mut writes = Vec::new();
                    for (entry_id, kind, pending) in items {
                        writes.push(Write::Value(set_value(&pending_entry(&entry_id), pending)));
                        projection.inbox.push(InboxItem { entry_id, kind });
                    }
                    writes.push(Write::Value(set_value(
                        &lane_state("main"),
                        json!({"currentOperationId": OPERATION_ID, "lastOperationId": null, "inbox": projection.inbox}),
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
        .expect("queue input");
}

async fn cancel_operation(fixture: &Fixture) {
    fixture
        .lane
        .command(
            move |mut projection, _| {
                Box::pin(async move {
                    let operation = projection
                        .operation
                        .as_mut()
                        .expect("fixture has no operation");
                    let mut state = serde_json::to_value(&operation.state).expect("state json");
                    state["control"] = json!({"status": "cancel_requested", "requestedAt": 2});
                    let state = decode_operation_state(state)?;
                    operation.state = state.clone();
                    Ok(LaneCommand::Commit {
                        decision: CommitDecision {
                            writes: vec![Write::Value(set_value(
                                &operation_state(OPERATION_ID),
                                serde_json::to_value(&state).expect("state json"),
                            ))],
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
        .expect("cancel operation");
}

fn settled(result: &ProcedureResult) -> Json {
    let ProcedureResult::Settled { outcome } = result else {
        panic!("expected settlement, got {result:?}")
    };
    serde_json::to_value(outcome).expect("outcome json")
}

fn compaction_hook(summary: &str) -> Json {
    json!({"summary": summary, "tokensBefore": 1_000, "retainedTail": [user("tail")]})
}

fn decline() -> HookResult {
    HookResult::Structural {
        decline: Some(true),
        value: None,
    }
}

fn provide(value: Json) -> HookResult {
    HookResult::Structural {
        decline: None,
        value: Some(value),
    }
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("structural procedure deadline")
}

fn deciding_of(state: OperationState) -> SummaryDecidingOperation {
    let OperationState::SummaryDeciding(deciding) = state else {
        panic!("expected summary.deciding, got {state:?}")
    };
    deciding
}

fn ready_of(state: OperationState) -> SummaryReadyOperation {
    let OperationState::SummaryReady(ready) = state else {
        panic!("expected summary.ready, got {state:?}")
    };
    ready
}

fn context_window(fixture: &Fixture) -> u64 {
    fixture
        .faux
        .get_model(None)
        .expect("faux model")
        .context_window
}

fn need_assistant_checkpoint(compaction: Json, trigger: &str) -> Json {
    with_scope(
        run_scope(compaction),
        json!({
            "at": "checkpoint",
            "continuation": {"kind": "need_assistant", "overflowRecoveryUsed": false},
            "triggerEntryId": trigger
        }),
    )
}

#[tokio::test]
async fn routes_a_declined_threshold_directly_to_assistant_generation() {
    let fixture = create_fixture().await;
    let checkpoint = need_assistant_checkpoint(
        json!({"enabled": true, "reserveTokens": context_window(&fixture), "keepRecentTokens": 1}),
        "assistant",
    );
    install_operation(
        &fixture,
        &checkpoint,
        json!({"kind": "run", "promptEntryIds": ["user"]}),
        Install::entries(vec![
            message_entry("user", None, user("question")),
            message_entry("assistant", Some("user"), assistant("answer")),
        ]),
    )
    .await;

    assert_eq!(
        bounded(run_checkpoint(
            fixture.lane.as_ref(),
            &fixture.drive,
            &decode(&checkpoint)
        ))
        .await
        .expect("checkpoint"),
        ProcedureResult::Continue
    );
    let deciding = deciding_of(fixture.current());
    assert!(matches!(
        deciding.task.boundary,
        ResultBoundary::ResumeCheckpoint { .. }
    ));
    assert!(
        fixture
            .value(&operation_preparation(OPERATION_ID, &deciding.task.task_id))
            .await
            .is_some()
    );

    fixture.on_hook(HookName::BeforeCompaction, |_| decline());
    assert_eq!(
        bounded(run_structural_decision(
            &fixture.lane,
            &fixture.drive,
            &deciding
        ))
        .await
        .expect("decision"),
        ProcedureResult::Continue
    );
    let routed = fixture.current_json();
    assert_eq!(routed["at"], "assistant.ready");
    assert_eq!(routed["generationContext"]["triggerEntryId"], "assistant");
    assert_eq!(routed["generationContext"]["overflowRecoveryUsed"], false);
    assert_eq!(fixture.count("compaction_start"), 1);
    assert_eq!(fixture.count("compaction_end"), 1);
}

#[tokio::test]
async fn uses_a_newer_compaction_entry_as_the_durable_threshold_guard() {
    let fixture = create_fixture().await;
    let window = context_window(&fixture);
    let checkpoint = need_assistant_checkpoint(
        json!({"enabled": true, "reserveTokens": window, "keepRecentTokens": 1}),
        "trigger",
    );
    install_operation(
        &fixture,
        &checkpoint,
        json!({"kind": "run", "promptEntryIds": ["trigger"]}),
        Install::entries(vec![
            message_entry("trigger", None, user("history")),
            json!({"id": "compacted", "parentId": "trigger", "type": "compaction", "summary": "already compacted", "retainedTail": [], "tokensBefore": window, "fromHook": false}),
        ]),
    )
    .await;

    assert_eq!(
        bounded(run_checkpoint(
            fixture.lane.as_ref(),
            &fixture.drive,
            &decode(&checkpoint)
        ))
        .await
        .expect("checkpoint"),
        ProcedureResult::Continue
    );
    let ready = fixture.current_json();
    assert_eq!(ready["at"], "assistant.ready");
    assert_eq!(ready["generationContext"]["triggerEntryId"], "trigger");
    assert_eq!(fixture.count("compaction_start"), 0);
}

#[tokio::test]
async fn rejects_a_missing_threshold_trigger_when_no_newer_compaction_guards_it() {
    let fixture = create_fixture().await;
    let checkpoint = need_assistant_checkpoint(
        json!({"enabled": true, "reserveTokens": context_window(&fixture), "keepRecentTokens": 1}),
        "missing-trigger",
    );
    install_operation(
        &fixture,
        &checkpoint,
        json!({"kind": "run", "promptEntryIds": []}),
        Install::entries(vec![message_entry("tip", None, user("history"))]),
    )
    .await;

    let error = bounded(run_checkpoint(
        fixture.lane.as_ref(),
        &fixture.drive,
        &decode(&checkpoint),
    ))
    .await
    .expect_err("missing trigger rejects");
    assert!(
        error
            .message
            .contains("Checkpoint trigger missing-trigger is missing from its Branch"),
        "{error:?}"
    );
}

fn may_finish_threshold(include_final_assistant: bool) -> Json {
    summary_deciding(
        default_scope(),
        run_compaction_task(
            "threshold",
            json!({"continuation": {"kind": "may_finish", "includeFinalAssistant": include_final_assistant}, "triggerEntryId": "tip"}),
        ),
    )
}

fn count_hook(fixture: &Fixture, name: HookName) -> Arc<std::sync::atomic::AtomicUsize> {
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = calls.clone();
    fixture.on_hook(name, move |_| {
        counter.fetch_add(1, Ordering::SeqCst);
        HookResult::None
    });
    calls
}

#[tokio::test]
async fn finishes_a_may_finish_run_directly_after_threshold_decline() {
    let fixture = create_fixture().await;
    let deciding = may_finish_threshold(false);
    install_operation(
        &fixture,
        &deciding,
        json!({"kind": "run", "promptEntryIds": ["tip"]}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    fixture.on_hook(HookName::BeforeCompaction, |_| decline());
    let finish_hooks = count_hook(&fixture, HookName::BeforeRunEnd);

    let result = bounded(run_structural_decision(
        &fixture.lane,
        &fixture.drive,
        &deciding_of(decode(&deciding)),
    ))
    .await
    .expect("decision");
    let outcome = settled(&result);
    assert_eq!(outcome["operationId"], OPERATION_ID);
    assert_eq!(outcome["kind"], "run");
    assert_eq!(outcome["status"], "completed");
    assert_eq!(outcome["tipId"], "tip");
    assert_eq!(fixture.storage.get_commit_attempts().len(), 1);
    assert_eq!(finish_hooks.load(Ordering::SeqCst), 1);
    assert!(fixture.lane.state().operation.is_none());
    let events = fixture.events();
    let tail = &events[events.len() - 2..];
    assert_eq!(tail[0]["type"], "compaction_end");
    assert_eq!(tail[0]["reason"], "threshold");
    assert_eq!(tail[0]["status"], "declined");
    assert_eq!(tail[1]["type"], "run_end");
    assert_eq!(tail[1]["status"], "completed");
}

#[tokio::test]
async fn routes_queued_follow_up_before_before_run_end_after_threshold_decline() {
    let fixture = create_fixture().await;
    let deciding = may_finish_threshold(false);
    install_operation(
        &fixture,
        &deciding,
        json!({"kind": "run", "promptEntryIds": ["tip"]}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    queue_input(
        &fixture,
        vec![(
            "follow-up",
            InboxItemKind::FollowUp,
            json!({"type": "message", "payload": user("continue")}),
        )],
    )
    .await;
    fixture.storage.clear_commit_attempts();
    fixture.on_hook(HookName::BeforeCompaction, |_| decline());
    let finish_hooks = count_hook(&fixture, HookName::BeforeRunEnd);

    assert_eq!(
        bounded(run_structural_decision(
            &fixture.lane,
            &fixture.drive,
            &deciding_of(decode(&deciding)),
        ))
        .await
        .expect("decision"),
        ProcedureResult::Continue
    );
    assert_eq!(fixture.storage.get_commit_attempts().len(), 1);
    assert_eq!(finish_hooks.load(Ordering::SeqCst), 0);
    let ready = fixture.current_json();
    assert_eq!(ready["at"], "assistant.ready");
    assert_eq!(ready["generationContext"]["triggerEntryId"], "follow-up");
    assert!(fixture.event_types().contains(&"compaction_end".to_owned()));
}

#[tokio::test]
async fn continues_to_an_assistant_turn_when_input_arrives_during_in_run_compaction() {
    for queue in [InboxItemKind::Steer, InboxItemKind::FollowUp] {
        let fixture = create_fixture().await;
        let deciding = may_finish_threshold(true);
        install_operation(
            &fixture,
            &deciding,
            json!({"kind": "run", "promptEntryIds": ["tip"]}),
            Install::prepared(
                vec![message_entry("tip", None, user("history"))],
                compaction_preparation(json!({})),
            ),
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
                        started.send(()).expect("started receiver");
                    }
                    if let Some(release) = release.lock().await.take() {
                        release.await.expect("release sender");
                    }
                    Ok(provide(compaction_hook("hook summary")))
                })
            }),
            None,
        );
        let lane = fixture.lane.clone();
        let drive = Drive::new(
            &crate::harness::agent_harness::DriveOptions {
                operation_id: OPERATION_ID.into(),
                wait_for_retry: None,
                poll_deferred: None,
            },
            &BACKGROUND_CONTEXT,
        );
        let state = deciding_of(decode(&deciding));
        let running = tokio::spawn(async move {
            let result = run_structural_decision(&lane, &drive, &state).await;
            (result, drive)
        });
        bounded(started_rx).await.expect("hook started");
        let label = if queue == InboxItemKind::Steer {
            "steer"
        } else {
            "followUp"
        };
        queue_input(
            &fixture,
            vec![(
                "queued",
                queue,
                json!({"type": "message", "payload": user(&format!("{label} during compaction"))}),
            )],
        )
        .await;
        release_tx.send(()).expect("release hook");
        let (result, drive) = bounded(running).await.expect("decision task");
        assert_eq!(
            result.expect("decision"),
            ProcedureResult::Continue,
            "{label}"
        );

        let routed = fixture.current_json();
        if queue == InboxItemKind::Steer {
            assert_eq!(routed["at"], "assistant.ready");
            assert_eq!(routed["generationContext"]["triggerEntryId"], "queued");
            assert_eq!(routed["generationContext"]["overflowRecoveryUsed"], false);
            assert!(fixture.lane.state().inbox.is_empty());
            assert!(fixture.value(&pending_entry("queued")).await.is_none());
            let entry = fixture.entry("queued").await.expect("queued entry");
            assert_eq!(entry["type"], "message");
            let parent = entry["parentId"].as_str().expect("queued parent");
            assert_eq!(
                fixture.entry(parent).await.expect("parent entry")["type"],
                "compaction"
            );
        } else {
            assert_eq!(routed["at"], "checkpoint");
            assert_eq!(
                fixture.lane.state().inbox,
                vec![InboxItem {
                    entry_id: "queued".into(),
                    kind: InboxItemKind::FollowUp
                }]
            );
            assert_eq!(
                bounded(run_checkpoint(
                    fixture.lane.as_ref(),
                    &drive,
                    &fixture.current()
                ))
                .await
                .expect("checkpoint"),
                ProcedureResult::Continue
            );
            let ready = fixture.current_json();
            assert_eq!(ready["at"], "assistant.ready");
            assert_eq!(ready["generationContext"]["triggerEntryId"], "queued");
            assert_eq!(ready["generationContext"]["overflowRecoveryUsed"], false);
            assert!(fixture.lane.state().inbox.is_empty());
        }
    }
}

#[tokio::test]
async fn publishes_structural_output_and_mixed_write_steer_input_in_one_admission_ordered_commit() {
    let fixture = create_fixture().await;
    let mut scope = default_scope();
    scope["settings"]["steeringMode"] = json!("one-at-a-time");
    let deciding = summary_deciding(
        scope,
        run_compaction_task(
            "threshold",
            json!({"continuation": {"kind": "need_assistant", "overflowRecoveryUsed": false}, "triggerEntryId": "tip"}),
        ),
    );
    install_operation(
        &fixture,
        &deciding,
        json!({"kind": "run", "promptEntryIds": ["tip"]}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    queue_input(
        &fixture,
        vec![
            (
                "write-1",
                InboxItemKind::Write,
                json!({"type": "custom", "customType": "note", "payload": {"order": 1}}),
            ),
            (
                "steer",
                InboxItemKind::Steer,
                json!({"type": "message", "payload": user("steer")}),
            ),
            (
                "write-2",
                InboxItemKind::Write,
                json!({"type": "custom", "customType": "note", "payload": {"order": 2}}),
            ),
            (
                "steer-2",
                InboxItemKind::Steer,
                json!({"type": "message", "payload": user("next steer")}),
            ),
        ],
    )
    .await;
    fixture.storage.clear_commit_attempts();
    fixture.on_hook(HookName::BeforeCompaction, |_| {
        provide(compaction_hook("summary"))
    });

    assert_eq!(
        bounded(run_structural_decision(
            &fixture.lane,
            &fixture.drive,
            &deciding_of(decode(&deciding)),
        ))
        .await
        .expect("decision"),
        ProcedureResult::Continue
    );
    assert_eq!(fixture.storage.get_commit_attempts().len(), 1);
    assert_eq!(
        fixture.lane.state().inbox,
        vec![InboxItem {
            entry_id: "steer-2".into(),
            kind: InboxItemKind::Steer
        }]
    );
    assert!(fixture.value(&pending_entry("steer-2")).await.is_some());
    let routed = fixture.current_json();
    assert_eq!(routed["at"], "assistant.ready");
    assert_eq!(routed["generationContext"]["triggerEntryId"], "steer");
    assert!(fixture.entry("write-1").await.expect("write-1")["parentId"].is_string());
    assert_eq!(
        fixture.entry("steer").await.expect("steer")["parentId"],
        "write-1"
    );
    assert_eq!(
        fixture.entry("write-2").await.expect("write-2")["parentId"],
        "steer"
    );
    assert_eq!(fixture.lane.state().tip_id.as_deref(), Some("write-2"));
}

#[tokio::test]
async fn queues_writes_during_standalone_structural_work_without_changing_the_operation() {
    let fixture = create_fixture().await;
    let deciding = summary_deciding(default_scope(), standalone_compaction_task());
    install_operation(
        &fixture,
        &deciding,
        json!({"kind": "compaction"}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;

    let entry_id = fixture
        .lane
        .append_custom_entry(
            "note".into(),
            Some(json!({"pending": true})),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("append custom");

    assert_eq!(fixture.current(), decode(&deciding));
    assert_eq!(fixture.lane.state().tip_id.as_deref(), Some("tip"));
    let queued = vec![InboxItem {
        entry_id: entry_id.clone(),
        kind: InboxItemKind::Write,
    }];
    assert_eq!(fixture.lane.state().inbox, queued);
    assert_eq!(
        fixture.value(&lane_state("main")).await,
        Some(json!({
            "currentOperationId": OPERATION_ID,
            "lastOperationId": null,
            "inbox": [{"entryId": entry_id, "kind": "write"}]
        }))
    );
    assert!(fixture.value(&pending_entry(&entry_id)).await.is_some());
    let last = fixture.events().last().cloned().expect("queue event");
    assert_eq!(last["type"], "queue_update");
    assert_eq!(last["queues"][0]["entryId"], json!(entry_id));
    assert_eq!(last["queues"][0]["kind"], "write");
    assert_eq!(last["queues"][0]["type"], "custom");
}

fn assistant_ready(fixture: &Fixture, compaction: Json) -> Json {
    with_scope(
        run_scope(compaction),
        json!({
            "at": "assistant.ready",
            "generationContext": {
                "stepId": "step",
                "triggerEntryId": "tip",
                "configuration": fixture.configuration,
                "streamOptions": {},
                "retryPolicy": retry_policy(2),
                "overflowRecoveryUsed": false
            },
            "nextAttempt": 1
        }),
    )
}

#[tokio::test]
async fn publishes_overflow_preparation_with_the_normalized_response_settlement() {
    let fixture = create_fixture().await;
    let ready = assistant_ready(
        &fixture,
        json!({"enabled": true, "reserveTokens": 1_000, "keepRecentTokens": 1}),
    );
    install_operation(
        &fixture,
        &ready,
        json!({"kind": "run", "promptEntryIds": ["tip"]}),
        Install::entries(vec![
            message_entry("question", None, user("earlier question")),
            message_entry("answer", Some("question"), assistant("answer")),
            message_entry("tip", Some("answer"), user("large prompt")),
        ]),
    )
    .await;
    fixture
        .faux
        .set_responses(vec![failure("prompt exceeds the context window").into()]);

    assert_eq!(
        bounded(run_generation(
            fixture.lane.as_ref(),
            &fixture.drive,
            &decode(&ready)
        ))
        .await
        .expect("generation"),
        ProcedureResult::Continue
    );
    let deciding = fixture.current_json();
    assert_eq!(deciding["at"], "summary.deciding");
    assert_eq!(deciding["task"]["reason"], "overflow");
    assert_eq!(deciding["task"]["boundary"]["kind"], "resume_checkpoint");
    assert_eq!(
        deciding["task"]["boundary"]["resumeAfter"]["continuation"],
        json!({"kind": "need_assistant", "overflowRecoveryUsed": true})
    );
    assert_eq!(
        deciding["task"]["boundary"]["resumeAfter"]["triggerEntryId"],
        "tip"
    );
    let task_id = deciding["task"]["taskId"].as_str().expect("task id");
    assert!(
        fixture
            .value(&operation_preparation(OPERATION_ID, task_id))
            .await
            .is_some()
    );
    let tip = fixture.lane.state().tip_id.expect("response tip");
    let response = fixture.entry(&tip).await.expect("response entry");
    assert_eq!(response["type"], "message");
    assert_eq!(response["message"]["stopReason"], "error");
    assert!(
        fixture
            .event_types()
            .contains(&"compaction_start".to_owned())
    );
}

#[tokio::test]
async fn fails_an_overflow_without_compacting_when_compaction_cannot_run() {
    for (name, compaction, with_history) in [
        (
            "nothing precedes the cut",
            json!({"enabled": true, "reserveTokens": 1_000, "keepRecentTokens": 1}),
            false,
        ),
        (
            "compaction is disabled",
            json!({"enabled": false, "reserveTokens": 1_000, "keepRecentTokens": 1}),
            true,
        ),
    ] {
        let fixture = create_fixture().await;
        let ready = assistant_ready(&fixture, compaction);
        let mut entries = Vec::new();
        if with_history {
            entries.push(message_entry("question", None, user("earlier question")));
            entries.push(message_entry(
                "answer",
                Some("question"),
                assistant("answer"),
            ));
        }
        let parent = entries
            .last()
            .map(|entry: &Json| entry["id"].as_str().expect("id").to_owned());
        entries.push(message_entry(
            "tip",
            parent.as_deref(),
            user("large prompt"),
        ));
        install_operation(
            &fixture,
            &ready,
            json!({"kind": "run", "promptEntryIds": ["tip"]}),
            Install::entries(entries),
        )
        .await;
        fixture
            .faux
            .set_responses(vec![failure("prompt exceeds the context window").into()]);

        bounded(run_generation(
            fixture.lane.as_ref(),
            &fixture.drive,
            &decode(&ready),
        ))
        .await
        .expect("generation");
        assert_eq!(fixture.count("compaction_start"), 0, "{name}");
        assert!(
            fixture
                .events()
                .iter()
                .any(|event| event["type"] == "run_end" && event["status"] == "failed"),
            "{name}"
        );
        assert_eq!(fixture.faux.state().call_count, 1, "{name}");
    }
}

#[tokio::test]
async fn does_not_compact_again_on_usage_measured_before_the_newest_compaction() {
    let fixture = create_fixture().await;
    let window = context_window(&fixture);
    let mut measured = assistant("answer");
    measured["usage"]["input"] = json!(window);
    measured["usage"]["totalTokens"] = json!(window);
    let checkpoint = need_assistant_checkpoint(
        json!({"enabled": true, "reserveTokens": 1_000, "keepRecentTokens": 1}),
        "next",
    );
    install_operation(
        &fixture,
        &checkpoint,
        json!({"kind": "run", "promptEntryIds": ["next"]}),
        Install::entries(vec![
            message_entry("question", None, user("question")),
            message_entry("answer", Some("question"), measured.clone()),
            json!({"id": "compacted", "parentId": "answer", "type": "compaction", "summary": "summary", "retainedTail": [user("question"), measured], "tokensBefore": window, "fromHook": false}),
            message_entry("next", Some("compacted"), user("next prompt")),
        ]),
    )
    .await;

    assert_eq!(
        bounded(run_checkpoint(
            fixture.lane.as_ref(),
            &fixture.drive,
            &decode(&checkpoint)
        ))
        .await
        .expect("checkpoint"),
        ProcedureResult::Continue
    );
    assert_eq!(fixture.current_json()["at"], "assistant.ready");
    assert_eq!(fixture.count("compaction_start"), 0);
}

#[tokio::test]
async fn starts_no_threshold_compaction_when_no_history_precedes_the_cut() {
    let fixture = create_fixture().await;
    let checkpoint = need_assistant_checkpoint(
        json!({"enabled": true, "reserveTokens": context_window(&fixture), "keepRecentTokens": 1_000_000}),
        "answer",
    );
    install_operation(
        &fixture,
        &checkpoint,
        json!({"kind": "run", "promptEntryIds": ["question"]}),
        Install::entries(vec![
            message_entry("question", None, user("question")),
            message_entry("answer", Some("question"), assistant("answer")),
        ]),
    )
    .await;

    assert_eq!(
        bounded(run_checkpoint(
            fixture.lane.as_ref(),
            &fixture.drive,
            &decode(&checkpoint)
        ))
        .await
        .expect("checkpoint"),
        ProcedureResult::Continue
    );
    assert_eq!(fixture.current_json()["at"], "assistant.ready");
    assert_eq!(fixture.count("compaction_start"), 0);
}

#[tokio::test]
async fn publishes_a_hook_compaction_and_terminal_cleanup_atomically_without_assistant_lifecycle() {
    let fixture = create_fixture().await;
    let deciding = summary_deciding(
        default_scope(),
        json!({"taskId": "task", "customInstructions": "focus", "boundary": {"kind": "finish"}}),
    );
    install_operation(
        &fixture,
        &deciding,
        json!({"kind": "compaction", "customInstructions": "focus"}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    let queued_id = fixture
        .lane
        .append_custom_entry(
            "retained".into(),
            Some(json!({"after": "compaction"})),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("append custom");
    fixture.on_hook(HookName::BeforeCompaction, |_| {
        let mut value = compaction_hook("hook summary");
        value["details"] = json!({"source": "hook"});
        provide(value)
    });

    let result = bounded(run_structural_decision(
        &fixture.lane,
        &fixture.drive,
        &deciding_of(decode(&deciding)),
    ))
    .await
    .expect("decision");
    let outcome = settled(&result);
    assert_eq!(outcome["operationId"], OPERATION_ID);
    assert_eq!(outcome["kind"], "compaction");
    assert_eq!(outcome["status"], "completed");
    let state = fixture.lane.state();
    assert!(state.operation.is_none());
    assert_eq!(
        state.inbox,
        vec![InboxItem {
            entry_id: queued_id.clone(),
            kind: InboxItemKind::Write
        }]
    );
    assert!(fixture.value(&pending_entry(&queued_id)).await.is_some());
    let entry = fixture
        .entry(state.tip_id.as_deref().expect("tip"))
        .await
        .expect("compaction entry");
    assert_eq!(entry["type"], "compaction");
    assert_eq!(entry["summary"], "hook summary");
    assert_eq!(entry["fromHook"], true);
    assert!(entry["seq"].is_number());
    assert_eq!(entry["timestamp"], 100);
    assert!(fixture.value(&operation_meta(OPERATION_ID)).await.is_none());
    assert!(
        fixture
            .value(&operation_preparation(OPERATION_ID, "task"))
            .await
            .is_none()
    );
    let types = fixture.event_types();
    assert!(
        !types
            .iter()
            .any(|kind| kind == "message_start" || kind == "message_end")
    );
    assert!(types.contains(&"entry_added".to_owned()));
    let last = fixture.events().last().cloned().expect("last event");
    assert_eq!(last["type"], "compaction_end");
    assert_eq!(last["reason"], "manual");
    assert_eq!(last["status"], "completed");
}

#[tokio::test]
async fn terminal_declines_standalone_compaction_without_publishing_an_entry() {
    let fixture = create_fixture().await;
    let deciding = summary_deciding(default_scope(), standalone_compaction_task());
    install_operation(
        &fixture,
        &deciding,
        json!({"kind": "compaction"}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    fixture.on_hook(HookName::BeforeCompaction, |_| decline());

    let result = bounded(run_structural_decision(
        &fixture.lane,
        &fixture.drive,
        &deciding_of(decode(&deciding)),
    ))
    .await
    .expect("decision");
    let outcome = settled(&result);
    assert_eq!(outcome["kind"], "compaction");
    assert_eq!(outcome["status"], "declined");
    assert_eq!(outcome["tipId"], "tip");
    assert!(fixture.lane.state().operation.is_none());
    assert_eq!(fixture.lane.state().tip_id.as_deref(), Some("tip"));
    let last = fixture.events().last().cloned().expect("last event");
    assert_eq!(last["type"], "compaction_end");
    assert_eq!(last["status"], "declined");
}

#[tokio::test]
async fn terminal_fails_overflow_decline_while_preserving_lane_owned_input() {
    let fixture = create_fixture().await;
    let deciding = summary_deciding(
        default_scope(),
        run_compaction_task(
            "overflow",
            json!({"continuation": {"kind": "need_assistant", "overflowRecoveryUsed": true}, "triggerEntryId": "tip"}),
        ),
    );
    install_operation(
        &fixture,
        &deciding,
        json!({"kind": "run", "promptEntryIds": ["tip"]}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    let queued_id = fixture
        .lane
        .append_custom_entry(
            "retained".into(),
            Some(json!({"value": true})),
            &BACKGROUND_CONTEXT,
        )
        .await
        .expect("append custom");
    fixture.on_hook(HookName::BeforeCompaction, |_| decline());

    let result = bounded(run_structural_decision(
        &fixture.lane,
        &fixture.drive,
        &deciding_of(decode(&deciding)),
    ))
    .await
    .expect("decision");
    let outcome = settled(&result);
    assert_eq!(outcome["kind"], "run");
    assert_eq!(outcome["status"], "failed");
    assert_eq!(outcome["error"]["code"], "compaction_declined");
    assert_eq!(
        fixture.lane.state().inbox,
        vec![InboxItem {
            entry_id: queued_id.clone(),
            kind: InboxItemKind::Write
        }]
    );
    assert!(fixture.value(&pending_entry(&queued_id)).await.is_some());
    let events = fixture.events();
    let tail = &events[events.len() - 2..];
    assert_eq!(tail[0]["type"], "compaction_end");
    assert_eq!(tail[0]["status"], "declined");
    assert_eq!(tail[1]["type"], "run_end");
    assert_eq!(tail[1]["status"], "failed");
}

#[tokio::test]
async fn gives_each_split_turn_provider_request_its_own_durable_intent_and_usage_row() {
    let fixture = create_fixture().await;
    let ready = summary_ready(
        default_scope(),
        standalone_compaction_task(),
        &fixture.configuration,
        2,
    );
    install_operation(
        &fixture,
        &ready,
        json!({"kind": "compaction"}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({
                "messagesToSummarize": [user("old history")],
                "turnPrefixMessages": [user("large turn")],
                "isSplitTurn": true
            })),
        ),
    )
    .await;
    fixture.faux.set_responses(vec![
        faux_assistant_message("history summary", Default::default()).into(),
        faux_assistant_message("turn prefix summary", Default::default()).into(),
    ]);

    let result = bounded(run_structural_generation(
        &fixture.lane,
        &fixture.drive,
        &ready_of(decode(&ready)),
    ))
    .await
    .expect("generation");
    let outcome = settled(&result);
    assert_eq!(outcome["kind"], "compaction");
    assert_eq!(outcome["status"], "completed");
    let writes = fixture.commit_writes();
    let request_indices: Vec<u64> = writes
        .iter()
        .filter(|write| {
            write["kind"] == "value" && write["op"] == "set" && write["namespace"] == "pi.op.state"
        })
        .filter_map(|write| write["value"]["request"]["index"].as_u64())
        .collect();
    assert_eq!(request_indices, vec![0, 1]);
    assert_eq!(
        writes
            .iter()
            .filter(|write| write["kind"] == "usage")
            .count(),
        2
    );
    assert!(!writes.iter().any(|write| {
        write["kind"] == "list" && write["namespace"] == "pi.pending.assistant_frame"
    }));
    assert!(!fixture.event_types().iter().any(|kind| {
        ["message_start", "message_update", "message_end"].contains(&kind.as_str())
    }));
    assert_eq!(fixture.count("usage"), 2);
}

#[tokio::test]
async fn preserves_the_overflow_recovery_bound_when_compaction_resumes_generation() {
    let fixture = create_fixture().await;
    let ready = summary_ready(
        default_scope(),
        run_compaction_task(
            "overflow",
            json!({"continuation": {"kind": "need_assistant", "overflowRecoveryUsed": true}, "triggerEntryId": "tip"}),
        ),
        &fixture.configuration,
        2,
    );
    install_operation(
        &fixture,
        &ready,
        json!({"kind": "run", "promptEntryIds": ["tip"]}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    fixture.faux.set_responses(vec![
        faux_assistant_message("generated summary", Default::default()).into(),
    ]);

    assert_eq!(
        bounded(run_structural_generation(
            &fixture.lane,
            &fixture.drive,
            &ready_of(decode(&ready)),
        ))
        .await
        .expect("generation"),
        ProcedureResult::Continue
    );
    let routed = fixture.current_json();
    assert_eq!(routed["at"], "assistant.ready");
    assert_eq!(routed["generationContext"]["triggerEntryId"], "tip");
    assert_eq!(routed["generationContext"]["overflowRecoveryUsed"], true);
    assert_eq!(
        fixture.lane.state().tip_id.as_deref(),
        Some("summary-entry")
    );
    let entry = fixture.entry("summary-entry").await.expect("summary entry");
    assert_eq!(entry["type"], "compaction");
    assert_eq!(entry["summary"], "generated summary");
    assert_eq!(entry["fromHook"], false);
}

#[tokio::test]
async fn settles_structural_usage_without_faulting_when_durable_cancellation_aborts_the_request() {
    let fixture = create_fixture().await;
    let ready = summary_ready(
        default_scope(),
        standalone_compaction_task(),
        &fixture.configuration,
        2,
    );
    install_operation(
        &fixture,
        &ready,
        json!({"kind": "compaction"}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    fixture
        .faux
        .set_responses(vec![FauxResponseStep::Factory(Arc::new(
            move |_: &maho_ai::types::Context,
                  options: Option<&maho_ai::types::SimpleStreamOptions>,
                  _: &maho_ai::providers::faux::FauxProviderState,
                  _: &maho_ai::types::Model| {
                let started = started_tx.lock().expect("started").take();
                let release = release_rx.clone();
                let signal = options.and_then(|options| options.stream.request.signal.clone());
                Box::pin(async move {
                    if let Some(started) = started {
                        started.send(()).expect("started receiver");
                    }
                    if let Some(release) = release.lock().await.take() {
                        release.await.expect("release sender");
                    }
                    let aborted = signal.as_ref().is_some_and(|signal| signal.aborted());
                    faux_assistant_message(
                        "",
                        FauxAssistantMessageOptions {
                            stop_reason: Some(if aborted {
                                maho_ai::types::StopReason::Aborted
                            } else {
                                maho_ai::types::StopReason::Stop
                            }),
                            error_message: aborted.then(|| "cancelled".to_owned()),
                            ..Default::default()
                        },
                    )
                })
            },
        ))]);

    let lane = fixture.lane.clone();
    let drive = Arc::new(Drive::new(
        &crate::harness::agent_harness::DriveOptions {
            operation_id: OPERATION_ID.into(),
            wait_for_retry: None,
            poll_deferred: None,
        },
        &BACKGROUND_CONTEXT,
    ));
    let running_drive = drive.clone();
    let state = ready_of(decode(&ready));
    let running =
        tokio::spawn(async move { run_structural_generation(&lane, &running_drive, &state).await });
    bounded(started_rx).await.expect("provider started");
    let cancellation = Cancellation::new();
    drive.begin_abort(cancellation.clone());
    cancel_operation(&fixture).await;
    cancellation.resolve();
    drive.signal_abort();
    release_tx.send(()).expect("release provider");

    assert_eq!(
        bounded(running)
            .await
            .expect("generation task")
            .expect("generation"),
        ProcedureResult::Continue
    );
    let cancelled = fixture.current_json();
    assert_eq!(cancelled["at"], "summary.effect_pending");
    assert_eq!(cancelled["control"]["status"], "cancel_requested");
    assert!(cancelled.get("request").is_none());
    assert_eq!(
        cancelled["usageIds"].as_array().expect("usage ids").len(),
        1
    );
    assert_eq!(
        fixture
            .commit_writes()
            .iter()
            .filter(|write| write["kind"] == "usage")
            .count(),
        1
    );
}

#[tokio::test]
async fn fails_missing_in_run_structural_models_with_configuration_provenance() {
    let fixture = create_fixture().await;
    let mut missing = fixture.configuration.clone();
    missing["model"] = json!({"provider": "missing", "modelId": "missing"});
    let ready = summary_ready(
        default_scope(),
        run_compaction_task(
            "threshold",
            json!({"continuation": {"kind": "need_assistant", "overflowRecoveryUsed": false}, "triggerEntryId": "tip"}),
        ),
        &missing,
        2,
    );
    install_operation(
        &fixture,
        &ready,
        json!({"kind": "run", "promptEntryIds": ["tip"]}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;

    let result = bounded(run_structural_generation(
        &fixture.lane,
        &fixture.drive,
        &ready_of(decode(&ready)),
    ))
    .await
    .expect("generation");
    let outcome = settled(&result);
    assert_eq!(outcome["kind"], "run");
    assert_eq!(outcome["status"], "failed");
    assert_eq!(outcome["error"]["code"], "model_unavailable");
    assert!(fixture.lane.state().operation.is_none());
    assert!(
        !fixture
            .commit_writes()
            .iter()
            .any(|write| write["kind"] == "usage")
    );

    let standalone = create_fixture().await;
    let mut missing = standalone.configuration.clone();
    missing["model"] = json!({"provider": "missing", "modelId": "missing"});
    let standalone_ready =
        summary_ready(default_scope(), standalone_compaction_task(), &missing, 2);
    install_operation(
        &standalone,
        &standalone_ready,
        json!({"kind": "compaction"}),
        Install::prepared(
            vec![message_entry("standalone-tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    let result = bounded(run_structural_generation(
        &standalone.lane,
        &standalone.drive,
        &ready_of(decode(&standalone_ready)),
    ))
    .await
    .expect("generation");
    let outcome = settled(&result);
    assert_eq!(outcome["kind"], "compaction");
    assert_eq!(outcome["status"], "failed");
    assert_eq!(outcome["error"]["code"], "model_unavailable");
    assert!(standalone.lane.state().operation.is_none());
}

/// Install a ready standalone compaction whose first attempt fails retryably, and return the
/// committed `summary.retry_wait` state.
async fn retry_wait_after_first_failure(fixture: &Fixture) -> SummaryRetryWaitOperation {
    let ready = summary_ready(
        default_scope(),
        standalone_compaction_task(),
        &fixture.configuration,
        2,
    );
    install_operation(
        fixture,
        &ready,
        json!({"kind": "compaction"}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    fixture
        .faux
        .set_responses(vec![failure("rate limit exceeded").into()]);
    assert_eq!(
        bounded(run_structural_generation(
            &fixture.lane,
            &fixture.drive,
            &ready_of(decode(&ready)),
        ))
        .await
        .expect("generation"),
        ProcedureResult::Continue
    );
    let OperationState::SummaryRetryWait(retry) = fixture.current() else {
        panic!("retryable failure did not wait")
    };
    assert_eq!(retry.retry.retry_wait.next_attempt, 2);
    retry
}

/// The pinned suite advances fake timers to `notBefore`; the Rust procedure reads the wall
/// clock, so the elapsed case rewrites the durable deadline to the past instead.
fn elapsed(retry: &SummaryRetryWaitOperation) -> SummaryRetryWaitOperation {
    let mut elapsed = retry.clone();
    elapsed.retry.retry_wait.not_before = 0;
    elapsed
}

#[tokio::test]
async fn durably_brackets_a_delayed_structural_retry_through_success() {
    let fixture = create_fixture().await;
    let mut retry = retry_wait_after_first_failure(&fixture).await;
    // A deadline far in the future keeps the not-yet-due branch independent of scheduling.
    retry.retry.retry_wait.not_before = i64::MAX;
    assert_eq!(
        bounded(run_structural_retry_wait(
            &fixture.lane,
            &fixture.drive,
            &retry
        ))
        .await
        .expect("retry wait"),
        ProcedureResult::Waiting {
            outcome: crate::harness::agent_harness::DriveOutcome::WaitingRetry {
                operation_id: OPERATION_ID.into(),
                not_before: i64::MAX,
            }
        }
    );
    assert_eq!(
        bounded(run_structural_retry_wait(
            &fixture.lane,
            &fixture.drive,
            &elapsed(&retry)
        ))
        .await
        .expect("retry wait"),
        ProcedureResult::Continue
    );
    let second = ready_of(fixture.current());
    fixture.faux.set_responses(vec![
        faux_assistant_message("summary", Default::default()).into(),
    ]);
    let result = bounded(run_structural_generation(
        &fixture.lane,
        &fixture.drive,
        &second,
    ))
    .await
    .expect("generation");
    assert_eq!(settled(&result)["status"], "completed");
    let retries: Vec<Json> = fixture
        .events()
        .into_iter()
        .filter(|event| {
            event["type"]
                .as_str()
                .is_some_and(|kind| kind.starts_with("retry_"))
        })
        .map(|event| match event.get("success") {
            Some(success) => json!({"type": event["type"], "success": success}),
            None => json!({"type": event["type"]}),
        })
        .collect();
    assert_eq!(
        retries,
        vec![
            json!({"type": "retry_scheduled"}),
            json!({"type": "retry_start"}),
            json!({"type": "retry_end", "success": true}),
        ]
    );
}

#[tokio::test]
async fn closes_an_exhausted_structural_retry_with_its_final_error() {
    let fixture = create_fixture().await;
    let retry = retry_wait_after_first_failure(&fixture).await;
    bounded(run_structural_retry_wait(
        &fixture.lane,
        &fixture.drive,
        &elapsed(&retry),
    ))
    .await
    .expect("retry wait");
    let second = ready_of(fixture.current());
    fixture
        .faux
        .set_responses(vec![failure("rate limit exceeded").into()]);

    let result = bounded(run_structural_generation(
        &fixture.lane,
        &fixture.drive,
        &second,
    ))
    .await
    .expect("generation");
    assert_eq!(settled(&result)["status"], "failed");
    let ends: Vec<Json> = fixture
        .events()
        .into_iter()
        .filter(|event| event["type"] == "retry_end")
        .collect();
    assert_eq!(ends.len(), 1);
    assert_eq!(ends[0]["attempt"], 2);
    assert_eq!(ends[0]["success"], false);
    assert!(
        ends[0]["finalError"]
            .as_str()
            .is_some_and(|error| error.contains("rate limit exceeded"))
    );
}

#[tokio::test]
async fn finishes_a_standalone_structural_failure_at_the_retry_cap() {
    let fixture = create_fixture().await;
    let ready = summary_ready(
        default_scope(),
        standalone_compaction_task(),
        &fixture.configuration,
        1,
    );
    install_operation(
        &fixture,
        &ready,
        json!({"kind": "compaction"}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;
    fixture
        .faux
        .set_responses(vec![failure("rate limit exceeded").into()]);

    let result = bounded(run_structural_generation(
        &fixture.lane,
        &fixture.drive,
        &ready_of(decode(&ready)),
    ))
    .await
    .expect("generation");
    let outcome = settled(&result);
    assert_eq!(outcome["operationId"], OPERATION_ID);
    assert_eq!(outcome["kind"], "compaction");
    assert_eq!(outcome["status"], "failed");
    assert_eq!(outcome["error"]["code"], "summarization_failed");
    assert!(fixture.lane.state().operation.is_none());
    assert!(
        !fixture
            .event_types()
            .contains(&"retry_scheduled".to_owned())
    );
}

#[tokio::test]
async fn rejects_invalid_unsummarized_navigation_state_without_committing() {
    for invalid in ["missing", "source", "root_label"] {
        let fixture = create_fixture().await;
        let target_id = match invalid {
            "missing" => json!("missing"),
            "source" => json!("source"),
            _ => Json::Null,
        };
        let mut navigation = with_scope(
            default_scope(),
            json!({"at": "navigation.ready_to_commit", "targetId": target_id}),
        );
        let mut intent = json!({"kind": "navigation", "targetId": target_id, "summarize": false});
        if invalid == "root_label" {
            navigation["label"] = json!("invalid");
            intent["label"] = json!("invalid");
        }
        install_operation(
            &fixture,
            &navigation,
            intent,
            Install::entries(vec![message_entry("source", None, user("source"))]).tip("source"),
        )
        .await;

        assert!(
            bounded(commit_navigation(fixture.lane.as_ref(), &fixture.drive))
                .await
                .is_err(),
            "{invalid}"
        );
        assert!(
            fixture.storage.get_commit_attempts().is_empty(),
            "{invalid}"
        );
        assert_eq!(fixture.lane.state().tip_id.as_deref(), Some("source"));
    }
}

fn navigation_entries() -> Vec<Json> {
    vec![
        message_entry("root", None, user("root")),
        message_entry("source", Some("root"), user("source")),
        message_entry("target", Some("root"), user("target")),
    ]
}

#[tokio::test]
async fn moves_an_unsummarized_navigation_and_cleans_up_in_one_terminal_transaction() {
    let fixture = create_fixture().await;
    let navigation = with_scope(
        default_scope(),
        json!({"at": "navigation.ready_to_commit", "targetId": "target", "label": "chosen"}),
    );
    install_operation(
        &fixture,
        &navigation,
        json!({"kind": "navigation", "targetId": "target", "summarize": false, "label": "chosen"}),
        Install::entries(navigation_entries()).tip("source"),
    )
    .await;

    let result = bounded(commit_navigation(fixture.lane.as_ref(), &fixture.drive))
        .await
        .expect("navigation");
    let outcome = settled(&result);
    assert_eq!(outcome["operationId"], OPERATION_ID);
    assert_eq!(outcome["kind"], "navigation");
    assert_eq!(outcome["status"], "completed");
    assert_eq!(outcome["fromTipId"], "source");
    assert_eq!(outcome["tipId"], "target");
    assert_eq!(fixture.lane.state().tip_id.as_deref(), Some("target"));
    assert_eq!(
        fixture
            .session
            .get_label("target", &BACKGROUND_CONTEXT)
            .await
            .expect("label"),
        Some("chosen".into())
    );
    let writes: Vec<Json> = fixture
        .storage
        .get_commit_attempts()
        .last()
        .expect("terminal commit")
        .iter()
        .map(|write| serde_json::to_value(write).expect("write json"))
        .collect();
    assert!(writes.iter().any(|write| {
        write["kind"] == "value" && write["namespace"] == "pi.op.state" && write["op"] == "delete"
    }));
    assert!(
        writes
            .iter()
            .any(|write| write["kind"] == "value" && write["namespace"] == "pi.result")
    );
    assert!(
        writes
            .iter()
            .any(|write| write["kind"] == "value" && write["namespace"] == "pi.lane.state")
    );
}

#[tokio::test]
async fn publishes_a_hook_navigation_summary_with_the_target_parent_and_source_identity() {
    let fixture = create_fixture().await;
    let deciding = summary_deciding(default_scope(), navigation_summary_task("target"));
    install_operation(
        &fixture,
        &deciding,
        json!({"kind": "navigation", "targetId": "target", "summarize": true}),
        Install::prepared(navigation_entries(), branch_preparation()).tip("source"),
    )
    .await;
    fixture.on_hook(HookName::BeforeNavigation, |_| {
        provide(json!({"summary": "branch summary", "readFiles": ["read.ts"], "modifiedFiles": ["edit.ts"]}))
    });

    let result = bounded(run_structural_decision(
        &fixture.lane,
        &fixture.drive,
        &deciding_of(decode(&deciding)),
    ))
    .await
    .expect("decision");
    let outcome = settled(&result);
    assert_eq!(outcome["kind"], "navigation");
    assert_eq!(outcome["status"], "completed");
    let tip = fixture.lane.state().tip_id.expect("tip");
    let entry = fixture.entry(&tip).await.expect("summary entry");
    assert_eq!(entry["type"], "branch_summary");
    assert_eq!(entry["parentId"], "target");
    assert_eq!(entry["fromId"], "source");
    assert_eq!(entry["summary"], "branch summary");
    assert_eq!(entry["fromHook"], true);
}

#[tokio::test]
async fn terminal_declines_summarized_navigation_without_moving_the_tip() {
    let fixture = create_fixture().await;
    let deciding = summary_deciding(default_scope(), navigation_summary_task("target"));
    install_operation(
        &fixture,
        &deciding,
        json!({"kind": "navigation", "targetId": "target", "summarize": true}),
        Install::prepared(navigation_entries(), branch_preparation()).tip("source"),
    )
    .await;
    fixture.on_hook(HookName::BeforeNavigation, |_| decline());

    let result = bounded(run_structural_decision(
        &fixture.lane,
        &fixture.drive,
        &deciding_of(decode(&deciding)),
    ))
    .await
    .expect("decision");
    let outcome = settled(&result);
    assert_eq!(outcome["kind"], "navigation");
    assert_eq!(outcome["status"], "declined");
    assert_eq!(outcome["tipId"], "source");
    assert_eq!(fixture.lane.state().tip_id.as_deref(), Some("source"));
    let last = fixture.events().last().cloned().expect("last event");
    assert_eq!(last["type"], "navigation_end");
    assert_eq!(last["status"], "declined");
}

#[tokio::test]
async fn generates_and_atomically_publishes_a_navigation_summary() {
    let fixture = create_fixture().await;
    let ready = summary_ready(
        default_scope(),
        navigation_summary_task("target"),
        &fixture.configuration,
        2,
    );
    install_operation(
        &fixture,
        &ready,
        json!({"kind": "navigation", "targetId": "target", "summarize": true}),
        Install::prepared(navigation_entries(), branch_preparation()).tip("source"),
    )
    .await;
    fixture.faux.set_responses(vec![
        faux_assistant_message("generated branch summary", Default::default()).into(),
    ]);

    let result = bounded(run_structural_generation(
        &fixture.lane,
        &fixture.drive,
        &ready_of(decode(&ready)),
    ))
    .await
    .expect("generation");
    let outcome = settled(&result);
    assert_eq!(outcome["kind"], "navigation");
    assert_eq!(outcome["status"], "completed");
    assert_eq!(outcome["fromTipId"], "source");
    assert_eq!(outcome["tipId"], "summary-entry");
    let entry = fixture.entry("summary-entry").await.expect("summary entry");
    assert_eq!(entry["type"], "branch_summary");
    assert_eq!(entry["parentId"], "target");
    assert_eq!(entry["fromId"], "source");
    assert_eq!(entry["fromHook"], false);
    assert_eq!(fixture.count("usage"), 1);
}

fn orphaned_effect(
    fixture: &Fixture,
    max_attempts: u32,
    request_index: u64,
    usage_ids: Json,
) -> Json {
    with_scope(
        default_scope(),
        json!({
            "at": "summary.effect_pending",
            "task": standalone_compaction_task(),
            "summaryContext": {
                "resultEntryId": "summary-entry",
                "configuration": fixture.configuration,
                "streamOptions": {},
                "retryPolicy": retry_policy(max_attempts)
            },
            "attempt": 1,
            "request": {"index": request_index, "usageId": "abandoned-usage"},
            "usageIds": usage_ids
        }),
    )
}

fn effect_of(state: OperationState) -> SummaryEffectPendingOperation {
    let OperationState::SummaryEffectPending(effect) = state else {
        panic!("expected summary.effect_pending, got {state:?}")
    };
    effect
}

#[tokio::test]
async fn consumes_an_orphaned_structural_attempt_and_never_resumes_its_nested_request() {
    let fixture = create_fixture().await;
    let effect = orphaned_effect(&fixture, 2, 1, json!(["settled-usage"]));
    install_operation(
        &fixture,
        &effect,
        json!({"kind": "compaction"}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;

    assert_eq!(
        bounded(recover_structural_generation(
            &fixture.lane,
            &fixture.drive,
            &effect_of(decode(&effect)),
        ))
        .await
        .expect("recovery"),
        ProcedureResult::Continue
    );
    let retry = fixture.current_json();
    assert_eq!(retry["at"], "summary.retry_wait");
    assert_eq!(retry["nextAttempt"], 2);
    assert!(retry.get("request").is_none());
    assert!(
        !fixture
            .commit_writes()
            .iter()
            .any(|write| write["kind"] == "usage")
    );
    let last = fixture.events().last().cloned().expect("last event");
    assert_eq!(last["type"], "retry_scheduled");
    assert_eq!(last["attempt"], 2);
    assert_eq!(last["recovery"], true);
}

#[tokio::test]
async fn terminal_fails_an_orphaned_structural_attempt_at_the_retry_cap() {
    let fixture = create_fixture().await;
    let effect = orphaned_effect(&fixture, 1, 0, json!([]));
    install_operation(
        &fixture,
        &effect,
        json!({"kind": "compaction"}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            compaction_preparation(json!({})),
        ),
    )
    .await;

    let result = bounded(recover_structural_generation(
        &fixture.lane,
        &fixture.drive,
        &effect_of(decode(&effect)),
    ))
    .await
    .expect("recovery");
    let outcome = settled(&result);
    assert_eq!(outcome["status"], "failed");
    assert_eq!(outcome["error"]["code"], "structural_interrupted");
    assert!(fixture.lane.state().operation.is_none());
    assert!(
        !fixture
            .event_types()
            .contains(&"retry_scheduled".to_owned())
    );
}

#[tokio::test]
async fn rejects_a_preparation_whose_durable_kind_contradicts_the_structural_state() {
    let fixture = create_fixture().await;
    let deciding = summary_deciding(default_scope(), standalone_compaction_task());
    install_operation(
        &fixture,
        &deciding,
        json!({"kind": "compaction"}),
        Install::prepared(
            vec![message_entry("tip", None, user("history"))],
            branch_preparation(),
        ),
    )
    .await;

    let error = bounded(run_structural_decision(
        &fixture.lane,
        &fixture.drive,
        &deciding_of(decode(&deciding)),
    ))
    .await
    .expect_err("contradicting preparation rejects");
    assert!(
        error
            .message
            .contains("Structural task task is missing its compaction preparation"),
        "{error:?}"
    );
}
