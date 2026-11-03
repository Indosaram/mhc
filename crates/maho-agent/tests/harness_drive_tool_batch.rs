//! Port of senpi `drive-tools.test.ts` (pin 903b8336): the durable tool batch driven end to end
//! through the production `Lane`, `ToolRunner`, and `runTools`.
//!
//! The pinned fixture records every storage commit and every emitted event into one shared
//! observation log (`ObservedMemoryStorage` + the lane's emit callback). The Rust fixture does the
//! same with a forwarding `Storage` decorator and bus listeners. Every interleaving is driven by
//! oneshot handshakes or by event waiters registered before the triggering action; every wait is
//! bounded.

use maho_agent::harness::agent_harness::{DriveOptions, ToolContextSource};
use maho_agent::harness::context::{BACKGROUND_CONTEXT, Context};
use maho_agent::harness::events::{HarnessEventBus, event_to_value};
use maho_agent::harness::execution::effect_gate::Cancellation;
use maho_agent::harness::hooks::{HookName, HookResult};
use maho_agent::harness::runtime::lane::{CommitDecision, Drive, Lane, LaneCommand};
use maho_agent::harness::runtime::restore::{decode_operation_state, restore_lane};
use maho_agent::harness::runtime::tools::ToolRunner;
use maho_agent::harness::runtime::types::{ProcedureResult, RuntimeDriveLane};
use maho_agent::harness::session::StorageDecorator;
use maho_agent::harness::session::commit::insert_entry;
use maho_agent::harness::session::session::SessionError;
use maho_agent::harness::session::types::*;
use maho_agent::harness::session::values::*;
use maho_agent::harness::session::{
    MemoryStorage, MemoryStorageOptions, StorageBackedSession, StorageBackedSessionOptions,
};
use maho_agent::harness::types::{
    AgentHarnessTool, AgentHarnessToolInvocation, AgentHarnessToolUpdateCallback,
    AgentHarnessToolUpdateOptions,
};
use maho_agent::types::{AgentToolResult, ReplayPolicy};
use maho_ai::providers::faux::{
    FauxAssistantMessageOptions, faux_assistant_message, faux_provider, faux_tool_call,
};
use maho_ai::types::{BoxFuture, ContentBlock, StopReason};
use serde_json::{Value as Json, json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(10);
const EVENT_TYPES: [&str; 12] = [
    "turn_start",
    "turn_end",
    "tool_start",
    "tool_update",
    "tool_end",
    "entry_added",
    "usage",
    "run_end",
    "value_update",
    "queue_update",
    "fault",
    "handler_error",
];

type Observations = Arc<Mutex<Vec<String>>>;
type EmitHook = Arc<dyn Fn(Json) -> BoxFuture<'static, ()> + Send + Sync>;
type EventWaiter = (
    Box<dyn Fn(&Json) -> bool + Send>,
    tokio::sync::oneshot::Sender<()>,
);

/// Pinned `ObservedMemoryStorage`: classify each landed commit into the shared observation log.
struct ObservedStorage {
    decorator: StorageDecorator,
    observations: Observations,
}

fn sets_namespace(writes: &[Write], namespace: &str) -> bool {
    writes.iter().any(
        |write| matches!(write, Write::Value(ValueWrite::Set(set)) if set.namespace == namespace),
    )
}

fn deletes_namespace(writes: &[Write], namespace: &str) -> bool {
    writes.iter().any(|write| {
        matches!(write, Write::Value(ValueWrite::Delete(delete)) if delete.namespace == namespace)
    })
}

impl Storage for ObservedStorage {
    fn commit<'a>(
        &'a self,
        writes: Vec<Write>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<CommitResult, SessionError>> {
        Box::pin(async move {
            let intent = sets_namespace(&writes, "pi.op.tool_args");
            let stages_outcome = sets_namespace(&writes, "pi.pending.entry");
            let replay = !stages_outcome && deletes_namespace(&writes, "pi.pending.tool_output");
            let result = self.decorator.commit(writes, context).await?;
            let mut observations = self.observations.lock().expect("observations");
            if intent {
                observations.push("intent_commit".into());
            }
            if stages_outcome {
                observations.push("outcome_commit".into());
            }
            if replay {
                observations.push("replay_commit".into());
            }
            Ok(result)
        })
    }
    fn get_entries<'a>(
        &'a self,
        ids: Vec<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<std::collections::BTreeMap<String, Entry>, SessionError>> {
        self.decorator.get_entries(ids, context)
    }
    fn get_value<'a>(
        &'a self,
        address: &'a Value,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Option<StoredValue>, SessionError>> {
        self.decorator.get_value(address, context)
    }
    fn scan_values<'a>(
        &'a self,
        prefix: &'a Value,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<StoredValue>, SessionError>> {
        self.decorator.scan_values(prefix, context)
    }
    fn read_list<'a>(
        &'a self,
        address: &'a ValueList,
        options: Option<ListReadOptions>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<ListElement>, SessionError>> {
        self.decorator.read_list(address, options, context)
    }
    fn scan_branch<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        self.decorator.scan_branch(query, context)
    }
    fn scan_branch_structure<'a>(
        &'a self,
        query: StorageBranchScan,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<EntryStructure>, SessionError>> {
        self.decorator.scan_branch_structure(query, context)
    }
    fn scan_entries<'a>(
        &'a self,
        query: EntryScan,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<Entry>, SessionError>> {
        self.decorator.scan_entries(query, context)
    }
    fn scan_usage<'a>(
        &'a self,
        query: UsageScan,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<UsageRow>, SessionError>> {
        self.decorator.scan_usage(query, context)
    }
    fn get_stats<'a>(
        &'a self,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<SessionStats, SessionError>> {
        self.decorator.get_stats(context)
    }
    fn close<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()> {
        self.decorator.close(context)
    }
}

type Execute = Arc<
    dyn Fn(
            String,
            AgentHarnessToolUpdateCallback,
            Arc<dyn AgentHarnessToolInvocation>,
            Context,
        ) -> BoxFuture<'static, Result<AgentToolResult, String>>
        + Send
        + Sync,
>;

/// Pinned `tool(name, execute, replay)` over the `{ value: string }` schema.
fn tool(name: &str, replay: ReplayPolicy, execute: Execute) -> Arc<AgentHarnessTool<()>> {
    Arc::new(AgentHarnessTool {
        label: name.into(),
        prepare_arguments: None,
        replay: Some(replay),
        tool: maho_ai::types::Tool {
            name: name.into(),
            description: name.into(),
            parameters: json!({
                "type": "object",
                "properties": {"value": {"type": "string"}},
                "required": ["value"]
            }),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(move |_, args, on_update, _, invocation, context| {
            let value = args["value"].as_str().unwrap_or_default().to_owned();
            execute(value, on_update, invocation, context)
        }),
    })
}

fn text_result(text: &str, details: Json) -> AgentToolResult {
    AgentToolResult {
        content: vec![ContentBlock::text(text)],
        details,
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    }
}

/// A tool whose execution only counts calls; the pinned `vi.fn(async () => ...)`.
fn counting_tool(
    name: &str,
    replay: ReplayPolicy,
    calls: &Arc<AtomicUsize>,
) -> Arc<AgentHarnessTool<()>> {
    let calls = calls.clone();
    tool(
        name,
        replay,
        Arc::new(move |_, _, _, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(text_result("", json!({}))) })
        }),
    )
}

fn counting_context(resolutions: &Arc<AtomicUsize>) -> ToolContextSource<()> {
    let resolutions = resolutions.clone();
    ToolContextSource::Resolve(Arc::new(move |_| {
        resolutions.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }))
}

type CallStatesHook = Box<dyn Fn(&[String]) -> Vec<Json>>;
type ExtraWritesHook = Box<dyn Fn(&str, &[String]) -> Vec<Write>>;

struct FixtureOptions {
    calls: Vec<&'static str>,
    tools: Vec<Arc<AgentHarnessTool<()>>>,
    mode: ToolExecutionMode,
    stop_reason: StopReason,
    call_states: Option<CallStatesHook>,
    extra_writes: Option<ExtraWritesHook>,
    tool_context: ToolContextSource<()>,
    cancelled: bool,
    on_emit: Option<EmitHook>,
}

impl Default for FixtureOptions {
    fn default() -> Self {
        Self {
            calls: Vec::new(),
            tools: Vec::new(),
            mode: ToolExecutionMode::Parallel,
            stop_reason: StopReason::ToolUse,
            call_states: None,
            extra_writes: None,
            // Pinned `toolContext: undefined` resolves to `undefined`; `()` is its typed stand-in.
            tool_context: ToolContextSource::Value(()),
            cancelled: false,
            on_emit: None,
        }
    }
}

struct Fixture {
    session: Arc<StorageBackedSession>,
    lane: Arc<Lane>,
    drive: Arc<Drive>,
    events: Arc<Mutex<Vec<Json>>>,
    observations: Observations,
    waiters: Arc<Mutex<Vec<EventWaiter>>>,
    assistant_entry_id: String,
    result_entry_ids: Vec<String>,
    operation_id: String,
    _subscriptions: Vec<maho_agent::harness::events::Unsubscribe>,
}

impl Fixture {
    fn events(&self) -> Vec<Json> {
        self.events.lock().expect("events").clone()
    }

    fn events_of(&self, kind: &str) -> Vec<Json> {
        self.events()
            .into_iter()
            .filter(|event| event["type"] == kind)
            .collect()
    }

    fn observation(&self, kind: &str) -> usize {
        self.observations
            .lock()
            .expect("observations")
            .iter()
            .position(|observed| observed == kind)
            .unwrap_or_else(|| panic!("missing observation {kind}"))
    }

    fn current(&self) -> Json {
        serde_json::to_value(
            self.lane
                .state()
                .operation
                .expect("fixture has no operation")
                .state,
        )
        .expect("state json")
    }

    fn statuses(&self) -> Vec<String> {
        self.current()["batch"]["calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .map(|call| call["status"].as_str().unwrap_or_default().to_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Register before the triggering action; resolves once a matching event is recorded.
    fn wait_event(
        &self,
        predicate: impl Fn(&Json) -> bool + Send + 'static,
    ) -> tokio::sync::oneshot::Receiver<()> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.waiters
            .lock()
            .expect("waiters")
            .push((Box::new(predicate), sender));
        receiver
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

    fn count_hook(&self, name: HookName) -> Arc<AtomicUsize> {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        self.on_hook(name, move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
            HookResult::None
        });
        calls
    }

    async fn value(&self, address: &Value) -> Option<Json> {
        self.session
            .get_value(address, &BACKGROUND_CONTEXT)
            .await
            .expect("value read")
            .map(|stored| stored.value)
    }

    async fn entry(&self, index: usize) -> Option<Json> {
        self.session
            .get_entry(&self.result_entry_ids[index], &BACKGROUND_CONTEXT)
            .await
            .expect("entry read")
            .map(|entry| serde_json::to_value(entry).expect("entry json"))
    }

    async fn transcript_ids(&self) -> Vec<String> {
        self.lane
            .find_entries(
                Some(BranchScan {
                    order: Some(BranchOrder::OldestFirst),
                    ..Default::default()
                }),
                &BACKGROUND_CONTEXT,
            )
            .await
            .expect("transcript")
            .into_iter()
            .map(|entry| entry.id)
            .collect()
    }

    async fn expect_projection_restores(&self) {
        assert_eq!(
            restore_lane(&*self.session, "main", &BACKGROUND_CONTEXT)
                .await
                .expect("restore lane"),
            self.lane.state()
        );
    }

    /// Pinned `driveTools(fixture)`, spawned so the test can interleave with it.
    fn spawn_drive(&self) -> tokio::task::JoinHandle<Result<ProcedureResult, SessionError>> {
        let lane = self.lane.clone();
        let drive = self.drive.clone();
        let state = self
            .lane
            .state()
            .operation
            .expect("fixture has no tool batch")
            .state;
        tokio::spawn(async move {
            RuntimeDriveLane::run_tools(lane.as_ref(), drive.as_ref(), state).await
        })
    }

    async fn drive_tools(&self) -> ProcedureResult {
        bounded(self.spawn_drive())
            .await
            .expect("drive task")
            .expect("run tools")
    }

    /// The pinned inline `lane.command` that commits `cancel_requested` for the open batch.
    async fn commit_cancel_requested(&self) {
        let operation_id = self.operation_id.clone();
        self.lane
            .command(
                move |mut state, _| {
                    Box::pin(async move {
                        let operation = state.operation.as_mut().expect("missing operation");
                        let mut next = serde_json::to_value(&operation.state).expect("state json");
                        next["control"] = json!({"status": "cancel_requested", "requestedAt": 40});
                        let next = decode_operation_state(next)?;
                        operation.state = next.clone();
                        Ok(LaneCommand::Commit {
                            decision: CommitDecision {
                                writes: vec![Write::Value(set_value(
                                    &operation_state(&operation_id),
                                    serde_json::to_value(&next).expect("state json"),
                                ))],
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
            .expect("commit cancellation");
    }
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(BOUND, future)
        .await
        .expect("bounded wait elapsed")
}

async fn create_fixture(options: FixtureOptions) -> Fixture {
    let observations: Observations = Arc::new(Mutex::new(Vec::new()));
    let storage = Arc::new(ObservedStorage {
        decorator: StorageDecorator::new(Arc::new(MemoryStorage::new(MemoryStorageOptions {
            now: Some(Arc::new(|| 100)),
        }))),
        observations: observations.clone(),
    });
    let session = Arc::new(StorageBackedSession::new(
        SessionMetadata {
            id: "drive-tools".into(),
            created_at: 1,
            storage_version: 1,
            cwd: None,
            parent_session_id: None,
            legacy_parent_session_path: None,
            path: None,
        },
        storage,
        StorageBackedSessionOptions::default(),
    ));
    session.attach();
    let next_id = |at: i64| (session.id_generator())(Some(at));
    let operation_id = next_id(10);
    let assistant_entry_id = next_id(20);
    let result_entry_ids: Vec<String> = options.calls.iter().map(|_| next_id(20)).collect();
    let faux = faux_provider(Default::default()).expect("faux provider");
    let model = faux.get_model(None).expect("faux model");
    let configuration = json!({
        "model": {"provider": model.provider, "modelId": model.id},
        "thinkingLevel": "off",
        "activeToolNames": options.calls
    });
    let assistant = faux_assistant_message(
        options
            .calls
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let mut arguments = serde_json::Map::new();
                arguments.insert("value".into(), json!(name));
                faux_tool_call(name, arguments, Some(format!("call-{index}").as_str()))
            })
            .collect::<Vec<_>>(),
        FauxAssistantMessageOptions {
            stop_reason: Some(options.stop_reason),
            timestamp: Some(20),
            ..Default::default()
        },
    );
    let calls = match &options.call_states {
        Some(states) => states(&result_entry_ids),
        None => result_entry_ids
            .iter()
            .enumerate()
            .map(|(index, id)| json!({"status": "planned", "sourceIndex": index, "resultEntryId": id}))
            .collect(),
    };
    let run = json!({
        "at": "tools",
        "control": if options.cancelled { json!({"status": "cancel_requested", "requestedAt": 30}) } else { json!({"status": "running"}) },
        "settings": {
            "compaction": maho_agent::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS,
            "steeringMode": "all",
            "followUpMode": "all",
            "toolExecution": options.mode
        },
        "batch": {"assistantEntryId": assistant_entry_id, "configuration": configuration, "turnId": "turn-1", "calls": calls},
        "latestAssistantEntryId": assistant_entry_id
    });
    let mut writes = vec![
        insert_entry(NewEntry {
            id: assistant_entry_id.clone(),
            parent_id: None,
            kind: EntryKind::Message {
                message: maho_agent::types::AgentMessage::from(maho_ai::types::Message::Assistant(
                    Arc::new(assistant),
                )),
                terminate: None,
            },
        }),
        Write::Value(set_value(&branch_tip("main"), json!(assistant_entry_id))),
        Write::Value(set_value(&lane_config("main"), configuration.clone())),
        Write::Value(set_value(
            &lane_state("main"),
            json!({"currentOperationId": operation_id, "lastOperationId": null, "inbox": []}),
        )),
        Write::Value(set_value(
            &operation_meta(&operation_id),
            json!({
                "operationId": operation_id,
                "lane": "main",
                "sourceTipId": null,
                "startedAt": 10,
                "intent": {"kind": "run", "promptEntryIds": []}
            }),
        )),
        Write::Value(set_value(&operation_state(&operation_id), run)),
    ];
    if let Some(extra) = &options.extra_writes {
        writes.extend(extra(&operation_id, &result_entry_ids));
    }
    let mutation = session
        .begin_mutation(&BACKGROUND_CONTEXT)
        .await
        .expect("seed mutation");
    mutation
        .commit(writes, &BACKGROUND_CONTEXT)
        .await
        .expect("seed batch");
    mutation.end(&BACKGROUND_CONTEXT).await;
    observations.lock().expect("observations").clear();

    let restored = restore_lane(&*session, "main", &BACKGROUND_CONTEXT)
        .await
        .expect("restore lane");
    let lane = Lane::new(
        "main".into(),
        session.clone(),
        restored,
        HarnessEventBus::new(),
    );
    lane.models.set_provider(faux.provider.clone());
    *lane.tool_runner.lock().expect("tool runner") =
        Arc::new(ToolRunner::new(options.tools, Some(options.tool_context)));
    let lane = Arc::new(lane);
    lane.install_self();
    let events = Arc::new(Mutex::new(Vec::new()));
    let waiters: Arc<Mutex<Vec<EventWaiter>>> = Arc::new(Mutex::new(Vec::new()));
    let subscriptions = EVENT_TYPES
        .iter()
        .map(|event_type| {
            let events = events.clone();
            let observations = observations.clone();
            let waiters = waiters.clone();
            let on_emit = options.on_emit.clone();
            lane.events.on(
                event_type,
                Arc::new(move |event, _| {
                    let value = event_to_value(&event);
                    events.lock().expect("events").push(value.clone());
                    observations
                        .lock()
                        .expect("observations")
                        .push(value["type"].as_str().unwrap_or_default().to_owned());
                    {
                        let mut waiters = waiters.lock().expect("waiters");
                        let pending = std::mem::take(&mut *waiters);
                        for (predicate, sender) in pending {
                            if predicate(&value) {
                                let _ = sender.send(());
                            } else {
                                waiters.push((predicate, sender));
                            }
                        }
                    }
                    let on_emit = on_emit.clone();
                    Box::pin(async move {
                        if let Some(on_emit) = on_emit {
                            on_emit(value).await;
                        }
                    })
                }),
            )
        })
        .collect();
    let drive = Arc::new(Drive::new(
        &DriveOptions {
            operation_id: operation_id.clone(),
            wait_for_retry: None,
            poll_deferred: None,
        },
        &BACKGROUND_CONTEXT,
    ));
    lane.set_active_drive(Some(drive.clone()));
    Fixture {
        session,
        lane,
        drive,
        events,
        observations,
        waiters,
        assistant_entry_id,
        result_entry_ids,
        operation_id,
        _subscriptions: subscriptions,
    }
}

fn last_text(entry: &Json) -> String {
    entry["message"]["content"]
        .as_array()
        .and_then(|content| content.last())
        .and_then(|block| block["text"].as_str())
        .unwrap_or_default()
        .to_owned()
}

#[tokio::test]
async fn executes_a_sequential_batch_with_memos_checkpoints_hooks_usage_and_source_order_placement()
{
    let context_resolutions = Arc::new(AtomicUsize::new(0));
    let late_invocation: Arc<Mutex<Option<Arc<dyn AgentHarnessToolInvocation>>>> =
        Arc::new(Mutex::new(None));
    let memo_round_trip = Arc::new(Mutex::new(None));
    let usage: maho_ai::types::Usage = serde_json::from_value(json!({
        "input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 3,
        "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0}
    }))
    .expect("usage");
    let first = {
        let late_invocation = late_invocation.clone();
        let memo_round_trip = memo_round_trip.clone();
        tool(
            "first",
            ReplayPolicy::Never,
            Arc::new(move |value, on_update, invocation, _| {
                *late_invocation.lock().expect("late") = Some(invocation.clone());
                let memo_round_trip = memo_round_trip.clone();
                Box::pin(async move {
                    invocation
                        .set_memo("step/a", Some(json!({"value": "memo"})))
                        .await;
                    *memo_round_trip.lock().expect("memo") = invocation.get_memo("step/a").await;
                    on_update(
                        text_result("partial", json!({"progress": "partial"})),
                        Some(AgentHarnessToolUpdateOptions {
                            checkpoint: Some(true),
                        }),
                    );
                    let mut result = text_result(&value, json!({"value": value}));
                    result.usage = Some(usage);
                    Ok(result)
                })
            }),
        )
    };
    let second = tool(
        "second",
        ReplayPolicy::Never,
        Arc::new(|value, _, _, _| {
            Box::pin(async move { Ok(text_result(&value, json!({"value": value}))) })
        }),
    );
    let fixture = create_fixture(FixtureOptions {
        calls: vec!["first", "second"],
        tools: vec![first, second],
        mode: ToolExecutionMode::Sequential,
        tool_context: counting_context(&context_resolutions),
        ..Default::default()
    })
    .await;
    fixture.on_hook(HookName::BeforeTool, |invocation| {
        if invocation.tool_name.as_deref() == Some("first") {
            let mut args = invocation.args.clone();
            args.insert("value".into(), json!("prepared"));
            HookResult::BeforeTool {
                args: Some(args),
                block: None,
            }
        } else {
            HookResult::None
        }
    });
    let checkpoint_seen = Arc::new(AtomicBool::new(false));
    {
        let seen = checkpoint_seen.clone();
        let session = fixture.session.clone();
        let address = pending_tool_output(&fixture.operation_id, &fixture.result_entry_ids[0]);
        let _ = fixture.lane.hooks.on(
            HookName::AfterTool,
            Arc::new(move |invocation, _| {
                let seen = seen.clone();
                let session = session.clone();
                let address = address.clone();
                Box::pin(async move {
                    if invocation.tool_name.as_deref() == Some("first") {
                        let stored = session
                            .get_value(&address, &BACKGROUND_CONTEXT)
                            .await
                            .map_err(|error| error.message)?;
                        seen.store(stored.is_some(), Ordering::SeqCst);
                    }
                    Ok(HookResult::None)
                })
            }),
            None,
        );
    }

    assert_eq!(fixture.drive_tools().await, ProcedureResult::Continue);
    let mut expected_ids = vec![fixture.assistant_entry_id.clone()];
    expected_ids.extend(fixture.result_entry_ids.clone());
    assert_eq!(fixture.transcript_ids().await, expected_ids);
    let first_result = fixture.entry(0).await.expect("first result");
    assert_eq!(first_result["message"]["role"], "toolResult");
    assert_eq!(first_result["message"]["content"][0]["text"], "prepared");
    assert_eq!(
        *memo_round_trip.lock().expect("memo"),
        Some(json!({"value": "memo"}))
    );
    assert_eq!(context_resolutions.load(Ordering::SeqCst), 1);
    assert!(checkpoint_seen.load(Ordering::SeqCst));
    let current = fixture.current();
    assert_eq!(current["at"], "checkpoint");
    assert_eq!(current["continuation"]["kind"], "need_assistant");
    assert_eq!(
        fixture.lane.state().configuration.active_tool_names,
        vec!["first".to_owned(), "second".to_owned()]
    );
    for prefix in [
        operation_tool_args_prefix(&fixture.operation_id, None),
        operation_tool_memo_prefix(&fixture.operation_id, None),
    ] {
        assert!(
            fixture
                .session
                .scan_values(&prefix, &BACKGROUND_CONTEXT)
                .await
                .expect("scan")
                .is_empty()
        );
    }
    assert!(
        fixture
            .value(&pending_tool_output(
                &fixture.operation_id,
                &fixture.result_entry_ids[0]
            ))
            .await
            .is_none()
    );
    // Pinned `lateInvocation.setMemo(...)` rejects "no longer owns"; the Rust capability is
    // inert once the effect settles, so the late write must not land.
    let late = late_invocation
        .lock()
        .expect("late")
        .clone()
        .expect("late invocation");
    late.set_memo("late", Some(json!(true))).await;
    assert!(
        fixture
            .value(&operation_tool_memo(
                &fixture.operation_id,
                &fixture.result_entry_ids[0],
                "late"
            ))
            .await
            .is_none()
    );
    assert_eq!(late.get_memo("step/a").await, None);
    let types: Vec<String> = fixture
        .events()
        .iter()
        .map(|event| event["type"].as_str().unwrap_or_default().to_owned())
        .collect();
    let index_of = |kind: &str| {
        types
            .iter()
            .position(|observed| observed == kind)
            .expect(kind)
    };
    assert!(index_of("tool_update") < index_of("tool_end"));
    let intent = fixture.observation("intent_commit");
    let start = fixture.observation("tool_start");
    let update = fixture.observation("tool_update");
    let commit = fixture.observation("outcome_commit");
    let end = fixture.observation("tool_end");
    let placement = fixture.observation("entry_added");
    assert!(intent < start);
    assert!(start < update);
    assert!(update < commit);
    assert!(commit < end);
    assert!(end < placement);
    assert_eq!(fixture.events_of("entry_added").len(), 2);
    assert_eq!(fixture.events_of("usage").len(), 1);
    assert_eq!(types.last().map(String::as_str), Some("turn_end"));
    fixture.expect_projection_restores().await;
}

#[tokio::test]
async fn stages_parallel_completion_order_but_materializes_source_order() {
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (finish_a_tx, finish_a_rx) = tokio::sync::oneshot::channel::<()>();
    let (finish_b_tx, finish_b_rx) = tokio::sync::oneshot::channel::<()>();
    let make = |name: &'static str, finish: tokio::sync::oneshot::Receiver<()>| {
        let started = started_tx.clone();
        let finish = Arc::new(tokio::sync::Mutex::new(Some(finish)));
        tool(
            name,
            ReplayPolicy::Never,
            Arc::new(move |_, _, _, _| {
                let started = started.clone();
                let finish = finish.clone();
                Box::pin(async move {
                    let _ = started.send(name.to_owned());
                    if let Some(finish) = finish.lock().await.take() {
                        let _ = finish.await;
                    }
                    Ok(text_result(name, json!({"value": name})))
                })
            }),
        )
    };
    let fixture = create_fixture(FixtureOptions {
        calls: vec!["a", "b"],
        tools: vec![make("a", finish_a_rx), make("b", finish_b_rx)],
        mode: ToolExecutionMode::Parallel,
        ..Default::default()
    })
    .await;
    let running = fixture.spawn_drive();
    let mut started = vec![
        bounded(started_rx.recv()).await.expect("first start"),
        bounded(started_rx.recv()).await.expect("second start"),
    ];
    started.sort();
    assert_eq!(started, ["a", "b"]);

    let b_staged =
        fixture.wait_event(|event| event["type"] == "tool_end" && event["toolName"] == "b");
    finish_b_tx.send(()).expect("finish b");
    bounded(b_staged).await.expect("b staged");
    assert_eq!(fixture.statuses(), ["effect_pending", "outcome_ready"]);
    assert!(fixture.entry(1).await.is_none());
    let start_b = fixture
        .events_of("tool_start")
        .into_iter()
        .find(|event| event["toolName"] == "b")
        .expect("tool_start b");
    assert_eq!(start_b["args"], json!({"value": "b"}));
    let end_b = fixture
        .events_of("tool_end")
        .into_iter()
        .find(|event| event["toolName"] == "b")
        .expect("tool_end b");
    assert_eq!(end_b["result"]["content"][0]["text"], "b");
    finish_a_tx.send(()).expect("finish a");
    bounded(running)
        .await
        .expect("drive task")
        .expect("run tools");

    let mut expected_ids = vec![fixture.assistant_entry_id.clone()];
    expected_ids.extend(fixture.result_entry_ids.clone());
    assert_eq!(fixture.transcript_ids().await, expected_ids);
    fixture.expect_projection_restores().await;
}

#[tokio::test]
async fn safe_replays_persisted_arguments_and_memos_while_interrupting_unsafe_effects() {
    let safe_calls = Arc::new(Mutex::new(Vec::<(String, String, Option<Json>)>::new()));
    let unsafe_calls = Arc::new(AtomicUsize::new(0));
    let safe = {
        let safe_calls = safe_calls.clone();
        tool(
            "safe",
            ReplayPolicy::Safe,
            Arc::new(move |value, _, invocation, _| {
                let safe_calls = safe_calls.clone();
                Box::pin(async move {
                    let memo = invocation.get_memo("step/a").await;
                    safe_calls.lock().expect("safe calls").push((
                        value,
                        invocation.invocation_id().to_owned(),
                        memo,
                    ));
                    Ok(text_result("safe replay", json!({"value": "safe"})))
                })
            }),
        )
    };
    let fixture = create_fixture(FixtureOptions {
        calls: vec!["safe", "unsafe"],
        tools: vec![safe, counting_tool("unsafe", ReplayPolicy::Never, &unsafe_calls)],
        call_states: Some(Box::new(|ids| {
            vec![
                json!({"status": "effect_pending", "sourceIndex": 0, "resultEntryId": ids[0], "replay": "safe"}),
                json!({"status": "effect_pending", "sourceIndex": 1, "resultEntryId": ids[1], "replay": "never"}),
            ]
        })),
        extra_writes: Some(Box::new(|operation_id, ids| {
            vec![
                Write::Value(set_value(
                    &operation_tool_args(operation_id, "turn-1", 0),
                    json!({"value": "persisted"}),
                )),
                Write::Value(set_value(
                    &operation_tool_args(operation_id, "turn-1", 1),
                    json!({"value": "unsafe"}),
                )),
                Write::Value(set_value(
                    &operation_tool_memo(operation_id, &ids[0], "step/a"),
                    json!({"complete": true}),
                )),
                Write::Value(set_value(
                    &pending_tool_output(operation_id, &ids[0]),
                    json!({"content": [{"type": "text", "text": "old progress"}], "details": {}}),
                )),
                Write::Value(set_value(
                    &pending_tool_output(operation_id, &ids[1]),
                    json!({"content": [{"type": "text", "text": "durable partial"}], "details": {"progress": "kept"}}),
                )),
            ]
        })),
        ..Default::default()
    })
    .await;

    fixture.drive_tools().await;
    let safe_calls = safe_calls.lock().expect("safe calls").clone();
    assert_eq!(safe_calls.len(), 1);
    assert_eq!(safe_calls[0].0, "persisted");
    assert!(!safe_calls[0].1.is_empty());
    assert_eq!(safe_calls[0].2, Some(json!({"complete": true})));
    assert_eq!(unsafe_calls.load(Ordering::SeqCst), 0);
    let start_0 = fixture
        .events_of("tool_start")
        .into_iter()
        .find(|event| event["toolCallId"] == "call-0")
        .expect("tool_start call-0");
    assert_eq!(start_0["recovery"], true);
    assert_eq!(start_0["args"], json!({"value": "persisted"}));
    assert!(fixture.observation("replay_commit") < fixture.observation("tool_start"));
    let end_0 = fixture
        .events_of("tool_end")
        .into_iter()
        .find(|event| event["toolCallId"] == "call-0")
        .expect("tool_end call-0");
    assert_eq!(end_0["recovery"], true);
    assert_eq!(end_0["isError"], false);
    let unsafe_entry = fixture.entry(1).await.expect("unsafe entry");
    assert_eq!(unsafe_entry["message"]["role"], "toolResult");
    assert_eq!(unsafe_entry["message"]["isError"], true);
    assert_eq!(
        unsafe_entry["message"]["details"],
        json!({"progress": "kept"})
    );
    assert!(last_text(&unsafe_entry).contains("external outcome is unknown"));
    assert!(
        !fixture
            .events_of("tool_start")
            .iter()
            .any(|event| event["toolCallId"] == "call-1")
    );
    let end_1 = fixture
        .events_of("tool_end")
        .into_iter()
        .find(|event| event["toolCallId"] == "call-1")
        .expect("tool_end call-1");
    assert_eq!(end_1["recovery"], true);
    assert_eq!(end_1["isError"], true);
}

#[tokio::test]
async fn reconciles_a_restored_cancelled_batch_without_hooks_context_or_effects() {
    let executions = Arc::new(AtomicUsize::new(0));
    let context_resolutions = Arc::new(AtomicUsize::new(0));
    let fixture = create_fixture(FixtureOptions {
        calls: vec!["planned", "pending"],
        tools: vec![
            counting_tool("planned", ReplayPolicy::Never, &executions),
            counting_tool("pending", ReplayPolicy::Safe, &executions),
        ],
        tool_context: counting_context(&context_resolutions),
        cancelled: true,
        call_states: Some(Box::new(|ids| {
            vec![
                json!({"status": "planned", "sourceIndex": 0, "resultEntryId": ids[0]}),
                json!({"status": "effect_pending", "sourceIndex": 1, "resultEntryId": ids[1], "replay": "safe"}),
            ]
        })),
        extra_writes: Some(Box::new(|operation_id, ids| {
            vec![
                Write::Value(set_value(
                    &operation_tool_args(operation_id, "turn-1", 1),
                    json!({"value": "pending"}),
                )),
                Write::Value(set_value(
                    &pending_tool_output(operation_id, &ids[1]),
                    json!({"content": [{"type": "text", "text": "checkpoint"}], "details": {}}),
                )),
            ]
        })),
        ..Default::default()
    })
    .await;
    let before_tool = fixture.count_hook(HookName::BeforeTool);
    let after_tool = fixture.count_hook(HookName::AfterTool);

    fixture.drive_tools().await;
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(context_resolutions.load(Ordering::SeqCst), 0);
    assert_eq!(before_tool.load(Ordering::SeqCst), 0);
    assert_eq!(after_tool.load(Ordering::SeqCst), 0);
    let current = fixture.current();
    assert_eq!(current["at"], "checkpoint");
    assert_eq!(current["control"]["status"], "cancel_requested");
    assert_eq!(current["continuation"]["kind"], "need_assistant");
    let pending = fixture.entry(1).await.expect("pending entry");
    assert!(last_text(&pending).contains("external outcome is unknown"));
    let starts = fixture.events_of("tool_start");
    let ends = fixture.events_of("tool_end");
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0]["toolName"], "planned");
    assert_eq!(starts[0]["args"], json!({"value": "planned"}));
    assert_eq!(ends.len(), 2);
    let planned_end = ends
        .iter()
        .find(|event| event["toolCallId"] == starts[0]["toolCallId"])
        .expect("planned tool_end");
    assert_eq!(planned_end["isError"], true);
}

#[tokio::test]
async fn materializes_outcome_ready_state_without_resolving_tools_or_tool_context() {
    let executions = Arc::new(AtomicUsize::new(0));
    let context_resolutions = Arc::new(AtomicUsize::new(0));
    let fixture = create_fixture(FixtureOptions {
        calls: vec!["ready"],
        tools: vec![counting_tool("ready", ReplayPolicy::Never, &executions)],
        tool_context: counting_context(&context_resolutions),
        call_states: Some(Box::new(|ids| {
            vec![json!({"status": "outcome_ready", "sourceIndex": 0, "resultEntryId": ids[0], "terminate": true})]
        })),
        extra_writes: Some(Box::new(|_, ids| {
            vec![Write::Value(set_value(
                &pending_entry(&ids[0]),
                json!({
                    "type": "message",
                    "payload": {
                        "role": "toolResult",
                        "toolCallId": "call-0",
                        "toolName": "ready",
                        "content": [{"type": "text", "text": "already done"}],
                        "isError": false,
                        "timestamp": 30
                    }
                }),
            ))]
        })),
        ..Default::default()
    })
    .await;

    fixture.drive_tools().await;
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert_eq!(context_resolutions.load(Ordering::SeqCst), 0);
    let current = fixture.current();
    assert_eq!(current["at"], "checkpoint");
    assert_eq!(
        current["continuation"],
        json!({"kind": "may_finish", "includeFinalAssistant": false})
    );
    assert_eq!(
        fixture.lane.state().configuration.active_tool_names,
        vec!["ready".to_owned()]
    );
    let events = fixture.events();
    assert_eq!(events[0]["type"], "turn_start");
    assert_eq!(events[0]["turnId"], "turn-1");
    assert_eq!(events[0]["recovery"], true);
    let last = events.last().expect("last event");
    assert_eq!(last["type"], "turn_end");
    assert_eq!(last["turnId"], "turn-1");
    assert_eq!(last["recovery"], true);
}

#[tokio::test]
async fn never_executes_genuine_length_or_missing_tool_calls() {
    let executions = Arc::new(AtomicUsize::new(0));
    let truncated = create_fixture(FixtureOptions {
        calls: vec!["present"],
        tools: vec![counting_tool("present", ReplayPolicy::Never, &executions)],
        stop_reason: StopReason::Length,
        ..Default::default()
    })
    .await;
    let truncated_after_tool = truncated.count_hook(HookName::AfterTool);
    truncated.drive_tools().await;
    assert_eq!(executions.load(Ordering::SeqCst), 0);
    let truncated_entry = truncated.entry(0).await.expect("truncated entry");
    assert!(
        truncated_entry["message"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .contains("arguments may be truncated")
    );
    assert_eq!(truncated_after_tool.load(Ordering::SeqCst), 0);
    assert_eq!(truncated.events_of("tool_start").len(), 1);
    assert_eq!(truncated.events_of("tool_end").len(), 1);
    assert!(truncated.observation("outcome_commit") < truncated.observation("tool_start"));
    assert!(truncated.observation("tool_start") < truncated.observation("tool_end"));
    assert!(truncated.observation("tool_end") < truncated.observation("entry_added"));

    let missing = create_fixture(FixtureOptions {
        calls: vec!["missing"],
        ..Default::default()
    })
    .await;
    let missing_after_tool = missing.count_hook(HookName::AfterTool);
    missing.drive_tools().await;
    let missing_entry = missing.entry(0).await.expect("missing entry");
    assert_eq!(missing_entry["message"]["role"], "toolResult");
    assert_eq!(missing_entry["message"]["isError"], true);
    assert_eq!(
        missing_entry["message"]["content"],
        json!([{"type": "text", "text": "Tool \"missing\" is unavailable"}])
    );
    assert!(missing_entry["message"].get("details").is_none());
    assert_eq!(missing_after_tool.load(Ordering::SeqCst), 0);
    let starts = missing.events_of("tool_start");
    assert_eq!(starts.len(), 1);
    assert_eq!(starts[0]["args"], json!({"value": "missing"}));
    let ends = missing.events_of("tool_end");
    assert_eq!(ends.len(), 1);
    assert_eq!(
        ends[0]["result"]["content"],
        json!([{"type": "text", "text": "Tool \"missing\" is unavailable"}])
    );
    assert!(ends[0]["result"]["details"].is_null());
    assert_eq!(ends[0]["isError"], true);
}

#[tokio::test]
async fn awaits_update_delivery_and_checkpoint_persistence_before_after_tool() {
    let (queued_tx, queued_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let queued_tx = Arc::new(Mutex::new(Some(queued_tx)));
    let release_rx = Arc::new(tokio::sync::Mutex::new(Some(release_rx)));
    let on_emit: EmitHook = Arc::new(move |event| {
        let queued = queued_tx.clone();
        let release = release_rx.clone();
        Box::pin(async move {
            if event["type"] != "tool_update" {
                return;
            }
            if let Some(queued) = queued.lock().expect("queued").take() {
                let _ = queued.send(());
            }
            if let Some(release) = release.lock().await.take() {
                let _ = release.await;
            }
        })
    });
    let fixture = create_fixture(FixtureOptions {
        calls: vec!["updating"],
        tools: vec![tool(
            "updating",
            ReplayPolicy::Never,
            Arc::new(|_, on_update, _, _| {
                on_update(
                    text_result("partial", json!({"progress": "partial"})),
                    Some(AgentHarnessToolUpdateOptions {
                        checkpoint: Some(true),
                    }),
                );
                Box::pin(async { Ok(text_result("done", json!({}))) })
            }),
        )],
        on_emit: Some(on_emit),
        ..Default::default()
    })
    .await;
    let after_started = Arc::new(AtomicBool::new(false));
    let checkpoint_seen = Arc::new(AtomicBool::new(false));
    {
        let after_started = after_started.clone();
        let checkpoint_seen = checkpoint_seen.clone();
        let session = fixture.session.clone();
        let address = pending_tool_output(&fixture.operation_id, &fixture.result_entry_ids[0]);
        let _ = fixture.lane.hooks.on(
            HookName::AfterTool,
            Arc::new(move |_, _| {
                after_started.store(true, Ordering::SeqCst);
                let checkpoint_seen = checkpoint_seen.clone();
                let session = session.clone();
                let address = address.clone();
                Box::pin(async move {
                    let stored = session
                        .get_value(&address, &BACKGROUND_CONTEXT)
                        .await
                        .map_err(|error| error.message)?;
                    checkpoint_seen.store(stored.is_some(), Ordering::SeqCst);
                    Ok(HookResult::None)
                })
            }),
            None,
        );
    }

    let running = fixture.spawn_drive();
    bounded(queued_rx).await.expect("update queued");
    // Pinned `setTimeout(0)`: give the drive a turn; it must still be parked on delivery.
    tokio::task::yield_now().await;
    assert!(!after_started.load(Ordering::SeqCst));
    release_tx.send(()).expect("release update");
    bounded(running)
        .await
        .expect("drive task")
        .expect("run tools");
    assert!(after_started.load(Ordering::SeqCst));
    assert!(checkpoint_seen.load(Ordering::SeqCst));
}

#[tokio::test]
async fn does_not_stage_a_cancelled_outcome_before_cancellation_is_durable() {
    let executions = Arc::new(AtomicUsize::new(0));
    let fixture = create_fixture(FixtureOptions {
        calls: vec!["cancel-before-admission"],
        tools: vec![counting_tool(
            "cancel-before-admission",
            ReplayPolicy::Never,
            &executions,
        )],
        mode: ToolExecutionMode::Sequential,
        ..Default::default()
    })
    .await;
    let cancellation = Cancellation::new();
    fixture.drive.begin_abort(cancellation.clone());
    let running = fixture.spawn_drive();
    // Pinned `setTimeout(0)`: the refused admission parks on the unresolved cancellation.
    tokio::task::yield_now().await;

    assert_eq!(fixture.statuses(), ["planned"]);
    assert!(
        fixture
            .value(&pending_entry(&fixture.result_entry_ids[0]))
            .await
            .is_none()
    );
    fixture.commit_cancel_requested().await;
    cancellation.resolve();
    fixture.drive.signal_abort();
    bounded(running)
        .await
        .expect("drive task")
        .expect("run tools");

    assert_eq!(executions.load(Ordering::SeqCst), 0);
    assert!(fixture.entry(0).await.is_some());
}

#[tokio::test]
async fn drains_live_updates_before_after_tool_and_stages_a_non_terminating_result_after_cancellation()
 {
    let (delivery_tx, delivery_rx) = tokio::sync::oneshot::channel::<()>();
    let delivery_rx = Arc::new(tokio::sync::Mutex::new(Some(delivery_rx)));
    let update_delivered = Arc::new(AtomicBool::new(false));
    let on_emit: EmitHook = {
        let update_delivered = update_delivered.clone();
        Arc::new(move |event| {
            let delivery = delivery_rx.clone();
            let update_delivered = update_delivered.clone();
            Box::pin(async move {
                if event["type"] != "tool_update" {
                    return;
                }
                if let Some(delivery) = delivery.lock().await.take() {
                    let _ = delivery.await;
                }
                update_delivered.store(true, Ordering::SeqCst);
            })
        })
    };
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let executing = tool(
        "slow",
        ReplayPolicy::Never,
        Arc::new(move |_, on_update, _, context| {
            on_update(text_result("partial", json!({"progress": "partial"})), None);
            if let Some(started) = started_tx.lock().expect("started").take() {
                let _ = started.send(());
            }
            Box::pin(async move {
                if let Some(signal) = context.abort_signal() {
                    signal.cancelled().await;
                }
                Err("cancelled effect".to_owned())
            })
        }),
    );
    let fixture = create_fixture(FixtureOptions {
        calls: vec!["slow"],
        tools: vec![executing],
        mode: ToolExecutionMode::Sequential,
        on_emit: Some(on_emit),
        ..Default::default()
    })
    .await;
    let after_tool_saw_delivery = Arc::new(Mutex::new(Vec::<bool>::new()));
    {
        let seen = after_tool_saw_delivery.clone();
        let update_delivered = update_delivered.clone();
        fixture.on_hook(HookName::AfterTool, move |_| {
            seen.lock()
                .expect("after tool")
                .push(update_delivered.load(Ordering::SeqCst));
            HookResult::None
        });
    }
    let running = fixture.spawn_drive();
    bounded(started_rx).await.expect("tool started");
    let cancellation = Cancellation::new();
    fixture.drive.begin_abort(cancellation.clone());
    fixture.commit_cancel_requested().await;
    cancellation.resolve();
    fixture.drive.signal_abort();
    delivery_tx.send(()).expect("release delivery");
    bounded(running)
        .await
        .expect("drive task")
        .expect("run tools");

    assert!(
        after_tool_saw_delivery
            .lock()
            .expect("after tool")
            .is_empty()
    );
    let entry = fixture.entry(0).await.expect("result entry");
    assert!(entry.get("terminate").is_none());
    assert_eq!(entry["message"]["role"], "toolResult");
    assert_eq!(entry["message"]["isError"], true);
}
