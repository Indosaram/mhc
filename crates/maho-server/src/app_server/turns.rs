use super::{projection::EventProjector, registry::{JsonRpcError, MethodRegistration, MethodScope}, server_core::ServerCore, thread_registry::{ThreadEntry, ThreadRegistry}, turn_log::{CompleteTurnOptions, CompleteTurnStatus, RecordTurnOptions, TurnLog}, turn_runtime::{build_turn, build_user_message, create_turn_id, parse_input}};
use maho_core::agent_session::{PromptDisposition, PromptOptions};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

/// senpi `PendingTurn`: the open turn `completeTurn` settles.
pub struct PendingTurn {
    pub turn_id: String,
    pub started_at_ms: f64,
    pub interrupted: bool,
    pub completed: bool,
    resolve: Option<tokio::sync::oneshot::Sender<()>>,
    /// senpi `PendingTurn.deferTerminalNotifications`: set by `turn/interrupt` so the idle status and
    /// terminal notifications follow its response.
    pub defer_terminal_notifications: Option<TerminalDeferral>,
}
/// Terminal emissions parked until the deferring request has been answered.
#[derive(Default)]
pub struct DeferredTerminal {
    responded: bool,
    parked: Vec<Box<dyn FnOnce() + Send>>,
}
pub type TerminalDeferral = Arc<std::sync::Mutex<DeferredTerminal>>;
/// senpi `TurnNotificationDeferral` bound to one request: emissions run once its response is sent.
fn terminal_deferral(connection: &super::registry::RegistryConnection) -> TerminalDeferral {
    let deferral: TerminalDeferral = Arc::default();
    let flush = deferral.clone();
    connection.defer_until_responded(move || {
        let parked = {
            let mut state = flush.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            state.responded = true;
            std::mem::take(&mut state.parked)
        };
        for emit in parked { emit(); }
    });
    deferral
}
/// One input of a thread's observer task. A `Complete` is queued behind every session event already
/// emitted, so a turn never finishes ahead of its own projected items; it only settles its own turn.
pub enum ObservedEvent {
    Session(Value),
    Complete { turn_id: String, status: CompleteTurnStatus, message: Option<String> },
}
const PREFLIGHT_FAILED: &str = "Prompt preflight failed";
/// senpi `TurnEngine` state shared by every handler and the per-thread observers.
#[derive(Clone)]
pub struct TurnEngine {
    core: std::sync::Weak<RwLock<ServerCore>>,
    log: Arc<Mutex<TurnLog>>,
}
fn now_iso(time: chrono::DateTime<chrono::Utc>) -> String { time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true) }

impl TurnEngine {
    pub fn new(core: std::sync::Weak<RwLock<ServerCore>>, log: Arc<Mutex<TurnLog>>) -> Self { Self { core, log } }

    /// senpi `observeThread` / `ensureSessionSubscription`: subscribe once per loaded thread so turns
    /// the client did not start (goal continuation, task wakeups, extension `sendUserMessage`) are
    /// projected, and every turn finishes on `agent_end`.
    pub async fn observe_thread(&self, entry: &Arc<Mutex<ThreadEntry>>) {
        let mut guard = entry.lock().await;
        if guard.observer.is_some() { return; }
        let (send, mut events) = tokio::sync::mpsc::unbounded_channel::<ObservedEvent>();
        let listener = send.clone();
        let subscription = guard.session.subscribe(Arc::new(move |event| {
            let value = match event {
                // The projector reads only the message role/responseId and the update's own fields, so
                // a streamed update skips encoding the accumulated message and its `partial` copy.
                maho_ext_api::AgentSessionEvent::Agent(maho_ext_api::AgentEvent::MessageUpdate { message, assistant_message_event }) => message_update_value(message, assistant_message_event),
                maho_ext_api::AgentSessionEvent::Agent(event) => serde_json::to_value(event),
                maho_ext_api::AgentSessionEvent::AgentEnd { .. } => Ok(json!({"type":"agent_end"})),
                // senpi `ensureSessionSubscription` forwards every session event; the projector turns
                // `compaction_start` / `compaction_end` into `contextCompaction` items.
                maho_ext_api::AgentSessionEvent::CompactionStart { .. } => Ok(json!({"type":"compaction_start"})),
                maho_ext_api::AgentSessionEvent::CompactionEnd { .. } => Ok(json!({"type":"compaction_end"})),
                _ => return,
            };
            match value {
                Ok(value) => { if listener.send(ObservedEvent::Session(value)).is_err() { eprintln!("app-server thread observer closed"); } },
                Err(error) => eprintln!("app-server event encoding failed: {error}"),
            }
        }));
        guard.observer = Some(subscription);
        guard.observer_events = Some(send);
        drop(guard);
        let engine = self.clone();
        let weak = Arc::downgrade(entry);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let Some(entry) = weak.upgrade() else { return; };
                match event {
                    ObservedEvent::Session(event) => {
                        engine.project_session_event(&entry, &event).await;
                        if event["type"] == "agent_end" {
                            if let Some(core) = engine.core.upgrade() {
                                let id = entry.lock().await.id.clone();
                                let core = core.read().await;
                                if let Some(approvals) = &core.approvals { approvals.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel_pending_for_thread(&id); }
                                if let Some(input) = &core.user_input { input.lock().unwrap_or_else(std::sync::PoisonError::into_inner).cancel_pending_for_thread(&id); }
                            }
                            engine.complete_turn(&entry, None, CompleteTurnStatus::Completed, None).await;
                        }
                    },
                    ObservedEvent::Complete { turn_id, status, message } => {
                        let gate = entry.lock().await.emit_gate.clone();
                        let _gate = gate.lock().await;
                        engine.complete_turn(&entry, Some(turn_id.as_str()), status, message).await;
                    },
                }
            }
        });
    }

    /// senpi `projectSessionEvent`.
    async fn project_session_event(&self, entry: &Arc<Mutex<ThreadEntry>>, event: &Value) {
        let gate = entry.lock().await.emit_gate.clone();
        let _gate = gate.lock().await;
        let active = entry.lock().await.active_turn.clone();
        let turn_id = match active {
            Some(turn_id) => turn_id,
            None => match self.start_extension_turn(entry, event).await { Some(turn_id) => turn_id, None => return },
        };
        let (thread_id, result) = {
            let mut guard = entry.lock().await;
            let thread_id = guard.id.clone();
            if guard.projection.as_ref().is_none_or(|(id, _)| *id != turn_id) {
                guard.projection = Some((turn_id.clone(), EventProjector::new(thread_id.clone(), turn_id.clone(), guard.cwd.clone())));
            }
            let result = guard.projection.as_mut().map(|(_, projector)| projector.project(event)).unwrap_or_default();
            (thread_id, result)
        };
        self.emit_projected(entry, &thread_id, &turn_id, result.notifications).await;
        if let Some(completion) = result.turn_completion && completion.status != "completed" && completion.status != "running" {
            let status = if completion.status == "interrupted" { CompleteTurnStatus::Interrupted } else { CompleteTurnStatus::Failed };
            self.complete_turn(entry, None, status, completion.error_message).await;
        }
    }

    /// senpi `startExtensionTurn`: a turn opened by the session itself (no `turn/start`).
    async fn start_extension_turn(&self, entry: &Arc<Mutex<ThreadEntry>>, event: &Value) -> Option<String> {
        let user_message = (event["type"] == "message_start" && event["message"]["role"] == "user").then(|| build_extension_user_message(&event["message"]));
        if user_message.is_none() && event["type"] != "agent_start" { return None; }
        let turn_id = create_turn_id();
        let now = chrono::Utc::now();
        let started = now.timestamp_millis() as f64;
        let thread_id = {
            let mut guard = entry.lock().await;
            guard.set_active_turn(Some(turn_id.clone()));
            guard.interrupted = false;
            guard.updated_at = now_iso(now);
            guard.pending_turn = Some(PendingTurn { turn_id: turn_id.clone(), started_at_ms: started, interrupted: false, completed: false, resolve: None, defer_terminal_notifications: None });
            guard.id.clone()
        };
        self.log.lock().await.record_turn(&thread_id, RecordTurnOptions { turn_id: turn_id.clone(), started_at: now_iso(now), status: None, completed_at: None, error: None });
        let turn = build_turn(&turn_id, "inProgress", started, None, &[], None);
        if let Some(core) = self.core.upgrade() {
            emit(&core, entry, json!({"method":"thread/status/changed","params":{"threadId":thread_id,"status":{"type":"active","activeFlags":[]}}})).await;
            emit(&core, entry, json!({"method":"turn/started","params":{"threadId":thread_id,"turn":turn}})).await;
        }
        if let Some(user) = user_message { self.emit_user_message(entry, &thread_id, &turn_id, started, user).await; }
        Some(turn_id)
    }

    /// senpi `completeTurn`; `expected` scopes it to one turn, like senpi's turn-id checks.
    pub async fn complete_turn(&self, entry: &Arc<Mutex<ThreadEntry>>, expected: Option<&str>, status: CompleteTurnStatus, message: Option<String>) {
        let (thread_id, mut pending, projection, interrupted) = {
            let mut guard = entry.lock().await;
            let open = guard.pending_turn.as_ref().is_some_and(|pending| !pending.completed
                && guard.active_turn.as_ref() == Some(&pending.turn_id)
                && expected.is_none_or(|expected| expected == pending.turn_id));
            if !open { return; }
            let interrupted = guard.interrupted || guard.pending_turn.as_ref().is_some_and(|pending| pending.interrupted);
            let Some(pending) = guard.pending_turn.take() else { return; };
            (guard.id.clone(), pending, guard.projection.take(), interrupted)
        };
        pending.completed = true;
        // Close dangling projected items first so they land on the wire and in the turn log
        // before turn/completed reads the logged items.
        if let Some((turn_id, mut projector)) = projection { self.emit_projected(entry, &thread_id, &turn_id, projector.finalize()).await; }
        let status = if interrupted && status == CompleteTurnStatus::Completed { CompleteTurnStatus::Interrupted } else { status };
        let completion = chrono::Utc::now();
        let items = {
            let mut log = self.log.lock().await;
            if let Err(error) = log.complete_turn(&thread_id, &pending.turn_id, CompleteTurnOptions { status, completed_at: now_iso(completion), error: message.clone() }) { eprintln!("app-server turn log: {error}"); }
            super::turn_runtime::read_logged_items(&mut log, &thread_id, &pending.turn_id)
        };
        let wire_status = match status { CompleteTurnStatus::Completed => "completed", CompleteTurnStatus::Failed => "failed", CompleteTurnStatus::Interrupted => "interrupted" };
        let turn = build_turn(&pending.turn_id, wire_status, pending.started_at_ms, Some(completion.timestamp_millis() as f64), &items, message.as_deref());
        { let mut guard = entry.lock().await; guard.set_active_turn(None); guard.interrupted = false; guard.updated_at = now_iso(completion); }
        if let Some(core) = self.core.upgrade() {
            let terminal_entry = entry.clone();
            let emit_terminal_notifications = async move {
                emit(&core, &terminal_entry, json!({"method":"thread/status/changed","params":{"threadId":thread_id,"status":{"type":"idle"}}})).await;
                for notification in super::turn_terminal::turn_terminal_notifications(&thread_id, turn) { emit(&core, &terminal_entry, notification).await; }
            };
            let mut emission = Some(emit_terminal_notifications);
            if let Some(deferral) = pending.defer_terminal_notifications.take() {
                let mut state = deferral.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if !state.responded && let Some(deferred) = emission.take() { state.parked.push(Box::new(move || { tokio::spawn(deferred); })); }
            }
            if let Some(emission) = emission { emission.await; }
        }
        if let Some(resolve) = pending.resolve.take() { let _ = resolve.send(()); }
    }

    /// senpi `index.ts` `interruptActiveTurns`: `interruptTurn` (abort, then `completeTurn` as
    /// interrupted, emitting its terminal notifications) for every loaded thread's active turn.
    pub async fn interrupt_active_turns(&self, threads: &ThreadRegistry) {
        let mut entries = Vec::new();
        for id in threads.loaded_ids().await {
            if let Ok(entry) = threads.get_loaded_thread(&id).await { entries.push(entry); }
        }
        futures_util::future::join_all(entries.iter().map(|entry| async move {
            let (session, active) = {
                let mut guard = entry.lock().await;
                let Some(active) = guard.active_turn.clone() else { return; };
                guard.interrupted = true;
                if let Some(pending) = guard.pending_turn.as_mut() && pending.turn_id == active { pending.interrupted = true; }
                (guard.session.clone(), active)
            };
            session.abort().await;
            let gate = entry.lock().await.emit_gate.clone();
            let _gate = gate.lock().await;
            self.complete_turn(entry, Some(active.as_str()), CompleteTurnStatus::Interrupted, None).await;
        })).await;
    }

    async fn emit_user_message(&self, entry: &Arc<Mutex<ThreadEntry>>, thread_id: &str, turn_id: &str, started_ms: f64, user: Value) {
        if let Some(core) = self.core.upgrade() {
            emit(&core, entry, json!({"method":"item/started","params":{"threadId":thread_id,"turnId":turn_id,"item":user,"startedAtMs":started_ms}})).await;
            emit(&core, entry, json!({"method":"item/completed","params":{"threadId":thread_id,"turnId":turn_id,"item":user,"completedAtMs":started_ms}})).await;
        }
        if let Some(item) = user.as_object() && let Err(error) = self.log.lock().await.append_item(thread_id, turn_id, item.clone()) { eprintln!("app-server turn log: {error}"); }
    }

    /// senpi `EventProjector.completed`: every `item/completed` reaches the wire, while the TurnLog
    /// append is deduplicated per turn by item id (`completedItemIds`).
    async fn emit_projected(&self, entry: &Arc<Mutex<ThreadEntry>>, thread_id: &str, turn_id: &str, notifications: Vec<Value>) {
        let Some(core) = self.core.upgrade() else { return; };
        for notification in notifications {
            if notification["method"] == "item/completed" && let Some(item) = notification["params"]["item"].as_object() {
                let mut log = self.log.lock().await;
                let logged = log.has_item(thread_id, turn_id, item.get("id").unwrap_or(&Value::Null));
                if !logged && let Err(error) = log.append_item(thread_id, turn_id, item.clone()) { eprintln!("app-server turn log: {error}"); }
            }
            emit(&core, entry, notification).await;
        }
    }
}

/// The `message_update` fields the projector reads, without the accumulated message or `partial`.
fn message_update_value(message: &maho_ext_api::AgentMessage, update: &maho_ai::types::AssistantMessageEvent) -> serde_json::Result<Value> {
    use maho_ai::types::AssistantMessageEvent as Update;
    let response_id = match message { maho_ext_api::AgentMessage::Llm(maho_ai::types::Message::Assistant(message)) => message.response_id.as_deref(), _ => None };
    let update = match update {
        Update::Start { .. } => json!({"type":"start"}),
        Update::TextStart { content_index, .. } => json!({"type":"text_start","contentIndex":content_index}),
        Update::TextDelta { content_index, delta, .. } => json!({"type":"text_delta","contentIndex":content_index,"delta":delta}),
        Update::TextEnd { content_index, content, .. } => json!({"type":"text_end","contentIndex":content_index,"content":content}),
        Update::ThinkingStart { content_index, .. } => json!({"type":"thinking_start","contentIndex":content_index}),
        Update::ThinkingDelta { content_index, delta, .. } => json!({"type":"thinking_delta","contentIndex":content_index,"delta":delta}),
        Update::ThinkingEnd { content_index, content, .. } => json!({"type":"thinking_end","contentIndex":content_index,"content":content}),
        Update::ToolcallStart { content_index, .. } => json!({"type":"toolcall_start","contentIndex":content_index}),
        Update::ToolcallDelta { content_index, delta, .. } => json!({"type":"toolcall_delta","contentIndex":content_index,"delta":delta}),
        Update::ToolcallEnd { content_index, tool_call, .. } => json!({"type":"toolcall_end","contentIndex":content_index,"toolCall":serde_json::to_value(tool_call)?}),
        Update::Done { reason, .. } => json!({"type":"done","reason":serde_json::to_value(reason)?}),
        Update::Error { reason, error } => json!({"type":"error","reason":serde_json::to_value(reason)?,"error":serde_json::to_value(error)?}),
    };
    Ok(json!({"type":"message_update","message":{"role":message.role(),"responseId":response_id},"assistantMessageEvent":update}))
}

/// senpi `buildExtensionUserMessage`.
fn build_extension_user_message(message: &Value) -> Value {
    let content = match &message["content"] {
        Value::String(text) => vec![json!({"type":"text","text":text,"text_elements":[]})],
        Value::Array(parts) => parts.iter().map(|part| if part["type"] == "text" {
            json!({"type":"text","text":part["text"],"text_elements":[]})
        } else {
            json!({"type":"image","url":format!("data:{};base64,{}", part["mimeType"].as_str().unwrap_or_default(), part["data"].as_str().unwrap_or_default())})
        }).collect(),
        _ => Vec::new(),
    };
    build_user_message(None, &content)
}

pub async fn register_turn_methods(core: &Arc<RwLock<ServerCore>>, threads: Arc<ThreadRegistry>, log: Arc<Mutex<TurnLog>>) -> TurnEngine {
    let weak = Arc::downgrade(core);
    let engine = TurnEngine::new(weak.clone(), log.clone());
    let mut core = core.write().await;
    for method in ["turn/steer","turn/interrupt"] {
        let threads = threads.clone();
        let log = log.clone(); let weak = weak.clone();
        core.registry.register(method.into(),MethodRegistration {requires_init:true,experimental:false,scope:MethodScope::Thread,handler:Arc::new(move |context| {
            let threads = threads.clone();
            let log = log.clone(); let weak = weak.clone();
            Box::pin(async move {
                let input_params = if method == "turn/steer" {Some(super::turn_adapter::turn_input_params(&context.request,true)?)} else {None};
                let (id,expected) = if let Some(params) = &input_params {(params.thread_id.clone(),params.expected_turn_id.clone().ok_or_else(||JsonRpcError::new(-32602,"Invalid params: expectedTurnId is required"))?)} else {super::turn_adapter::turn_interrupt_params(&context.request)?};
                let id = id.as_str();
                let entry = threads.get_loaded_thread(id).await.map_err(|error|JsonRpcError::new(-32600,error))?;
                let (session,active) = {let entry = entry.lock().await;(entry.session.clone(),entry.active_turn.clone())};
                let Some(active) = active else {
                    return if method == "turn/interrupt" {Ok(json!({}))} else {Err(JsonRpcError::new(-32600,format!("No active turn for thread {id}")))};
                };
                if expected != active {return Err(JsonRpcError::new(-32600,format!("Turn id mismatch: expected {expected} but active turn is {active}")));}
                if method == "turn/interrupt" {
                    let observer = {
                        let mut guard = entry.lock().await;
                        guard.interrupted = true;
                        // senpi `interruptTurn`: `pending.deferTerminalNotifications` is set before
                        // `abort()`, so a completion on `agent_end` still follows the response.
                        if let Some(pending) = guard.pending_turn.as_mut() && pending.turn_id == active {
                            pending.interrupted = true;
                            pending.defer_terminal_notifications = Some(terminal_deferral(&context.connection));
                        }
                        guard.observer_events.clone()
                    };
                    session.abort().await;
                    // Queued behind the already-observed events; settles only if the turn is still open.
                    if let Some(observer) = observer && observer.send(ObservedEvent::Complete { turn_id:active, status:CompleteTurnStatus::Interrupted, message:None }).is_err() { eprintln!("app-server thread observer closed"); }
                    return Ok(json!({}));
                }
                let params = input_params.ok_or_else(||JsonRpcError::new(-32602,"Invalid params"))?;
                let parsed = parse_input(&params.input)?;
                session.steer(&parsed.text,None,maho_core::agent_session::QueuedInputOptions {source:Some(maho_ext_api::InputSource::Rpc),..Default::default()}).await.map_err(|error|JsonRpcError::new(-32603,error))?;
                let user = build_user_message(params.client_user_message_id.as_deref(),&parsed.content);
                log.lock().await.append_item(id,&active,user.as_object().cloned().ok_or_else(||JsonRpcError::new(-32603,"Invalid user message"))?).map_err(|error|JsonRpcError::new(-32603,error.to_string()))?;
                if let Some(core) = weak.upgrade() {
                    let now = chrono::Utc::now().timestamp_millis();
                    emit(&core,&entry,json!({"method":"item/started","params":{"threadId":id,"turnId":active,"item":user,"startedAtMs":now}})).await;
                    emit(&core,&entry,json!({"method":"item/completed","params":{"threadId":id,"turnId":active,"item":user,"completedAtMs":now}})).await;
                }
                Ok(json!({"turnId":active}))
            })
        })});
    }
    let start_engine = engine.clone();
    core.registry.register("turn/start".into(), MethodRegistration { requires_init: true, experimental: false, scope: MethodScope::Thread, handler: Arc::new(move |context| {
        let threads = threads.clone(); let log = log.clone(); let engine = start_engine.clone();
        Box::pin(async move {
            let params = super::turn_adapter::turn_input_params(&context.request,false)?;
            let id = params.thread_id;
            // senpi `startTurn`: `getLoadedThreadOrThrow` runs before `parseInput`.
            let entry = threads.get_loaded_thread(&id).await.map_err(|error|JsonRpcError::new(-32600,error))?;
            let parsed = parse_input(&params.input)?;
            engine.observe_thread(&entry).await;
            let (session, tasks, gate) = { let entry = entry.lock().await; (entry.session.clone(), entry.tasks.clone(), entry.emit_gate.clone()) };
            let guard = tasks.lock_owned().await;
            let gate = gate.lock_owned().await;
            let turn_id = create_turn_id(); let now = chrono::Utc::now(); let started = now.timestamp_millis() as f64;
            let (resolve, completion) = tokio::sync::oneshot::channel::<()>();
            {
                let mut entry = entry.lock().await;
                entry.set_active_turn(Some(turn_id.clone())); entry.interrupted = false; entry.updated_at = now_iso(now);
                entry.pending_turn = Some(PendingTurn { turn_id: turn_id.clone(), started_at_ms: started, interrupted: false, completed: false, resolve: Some(resolve), defer_terminal_notifications: None });
            }
            log.lock().await.record_turn(&id, RecordTurnOptions { turn_id:turn_id.clone(), started_at:now_iso(now), status:None, completed_at:None, error:None });
            let user = build_user_message(params.client_user_message_id.as_deref(), &parsed.content);
            let turn = build_turn(&turn_id,"inProgress",started,None,&[],None);
            // senpi `preflightResult`: turn/start resolves only once the prompt is admitted; a
            // rejection before admission rejects the request.
            let (admitted, admission) = tokio::sync::oneshot::channel::<()>();
            let admitted = std::sync::Mutex::new(Some(admitted));
            let was_admitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let admitted_flag = was_admitted.clone();
            let prompt_session = session.clone();
            let observer = entry.lock().await.observer_events.clone();
            let prompt_turn = turn_id.clone();
            let prompt = tokio::spawn(async move {
                let result = prompt_session.prompt(&parsed.text, PromptOptions { source:Some(maho_ext_api::InputSource::Rpc), prompt_admitted:Some(Arc::new(move |_disposition: PromptDisposition| {
                    admitted_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    if let Some(admitted) = admitted.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take() { let _ = admitted.send(()); }
                })), ..Default::default() }).await;
                // After admission the agent run (and its `agent_end`) owns completion; a handled
                // command never starts a run, and a hard failure ends the turn as failed.
                let completion = match &result {
                    Ok(PromptDisposition::Handled) => Some((CompleteTurnStatus::Completed, None)),
                    Err(error) => Some((CompleteTurnStatus::Failed, Some(error.clone()))),
                    Ok(_) => None,
                };
                let completion = completion.map(|(status, message)| ObservedEvent::Complete { turn_id:prompt_turn, status, message });
                if was_admitted.load(std::sync::atomic::Ordering::SeqCst) && let Some(completion) = completion && let Some(observer) = observer && observer.send(completion).is_err() { eprintln!("app-server thread observer closed"); }
                result
            });
            let preflight = match admission.await {
                Ok(()) => Ok(()),
                Err(_) => Err(match prompt.await {
                    Ok(Err(error)) => error,
                    Ok(Ok(_)) => PREFLIGHT_FAILED.to_owned(),
                    Err(error) => error.to_string(),
                }),
            };
            let response = preflight.as_ref().map(|_| json!({"turn":turn.clone()})).map_err(|error| JsonRpcError::new(-32603, error.clone()));
            if preflight.is_err() {
                // senpi `preflightResult(false)`: `completeTurn("failed")` runs before the request
                // rejects, and the deferred initial notifications are dropped with the error response.
                engine.complete_turn(&entry, Some(turn_id.as_str()), CompleteTurnStatus::Failed, Some(PREFLIGHT_FAILED.to_owned())).await;
                drop(gate);
                drop(guard);
                return response;
            }
            let initial_entry = entry.clone(); let initial_engine = engine.clone(); let initial_id = id.clone(); let initial_turn = turn_id.clone();
            context.connection.defer_until_responded(move || { tokio::spawn(async move {
                let _guard = guard;
                if let Some(core) = initial_engine.core.upgrade() {
                    emit(&core,&initial_entry,json!({"method":"thread/status/changed","params":{"threadId":initial_id,"status":{"type":"active","activeFlags":[]}}})).await;
                    emit(&core,&initial_entry,json!({"method":"turn/started","params":{"threadId":initial_id,"turn":turn}})).await;
                }
                initial_engine.emit_user_message(&initial_entry, &initial_id, &initial_turn, started, user).await;
                drop(gate);
                let _ = completion.await;
            }); });
            response
        })
    }) });
    engine
}
pub(super) async fn emit(core: &Arc<RwLock<ServerCore>>, entry: &Arc<Mutex<ThreadEntry>>, notification: Value) {
    if notification["method"] == "thread/status/changed" {
        if let Err(error) = core.read().await.broadcast_notification(notification,chrono::Utc::now().timestamp_millis() as u64).await { eprintln!("app-server notification: {}",error.message); }
        return;
    }
    let subscribers = entry.lock().await.subscribers.clone();
    let mut delivered = false;
    // senpi `NotificationRouter.toThread`: every initialized subscriber counts as routed, even
    // when its opt-outs or the experimental filter drop the message.
    for id in subscribers {
        match core.read().await.send_thread_notification_to_connection(&id,notification.clone(),chrono::Utc::now().timestamp_millis() as u64).await {
            Ok(sent) => delivered |= sent,
            Err(error) => eprintln!("app-server notification: {}",error.message),
        }
    }
    if !delivered && matches!(notification["method"].as_str(),Some("turn/completed"|"error")) {
        let mut entry = entry.lock().await;
        entry.queued_terminal_notifications.push(notification);
        if entry.queued_terminal_notifications.len() > 100 { entry.queued_terminal_notifications.remove(0); }
    }
}
