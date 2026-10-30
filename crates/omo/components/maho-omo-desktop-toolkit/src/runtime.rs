//! Port of `omo-senpi/src/components/desktop-toolkit/runtime.ts` at omo v5.1.52: the lazy runtime
//! behind the toolkit component. It answers `omo_desktop_toolkit` acquisitions, discovers the
//! desktop thread MCP once the app offers the `thread` toolkit, registers its tools as
//! `desktop_<tool>` with a per-caller grant, and retries a failed discovery in the background on
//! later turns with a capped backoff.

use maho_ai::utils::abort::{AbortController, AbortSignal};
use maho_ext_api::{
    EventKind, EventResult, ExtensionApi, ExtensionFuture, ExtensionContext, ExtensionEvent,
    ExtensionHandlerOptions, ExtensionToolExecutor, ToolDefinition, ToolExecutionMode,
    ToolExposure, ToolSessionManager,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::Duration,
};
use tokio::sync::{oneshot, watch};

use crate::thread_tools::{call_thread_tool, list_thread_tools};
use crate::{
    DESKTOP_TOOLKIT_TOOL_NAME, TOOLKIT_GRANT_RPC, TOOLKIT_REQUEST_EVENT, ToolkitGrant,
    offered_toolkits, parse_toolkit_grant, reply,
};

/// Turns to wait after the 1st, 2nd and 3rd failed attempt before the next background retry. After
/// the 4th failure the session stops asking: an app that never offers the grant costs at most three
/// retries, not one request per turn forever.
pub const RETRY_GAPS_IN_TURNS: [u32; 3] = [1, 2, 4];

type Settled = Result<ToolkitGrant, String>;

pub(crate) fn acquire_parameters() -> Value {
    json!({
        "type": "object",
        "properties": {"op": {"type": "string", "enum": ["acquire"]}, "toolkit": {"type": "string"}},
        "required": ["op", "toolkit"],
        "additionalProperties": false,
    })
}

fn none<'a>() -> ExtensionFuture<'a, EventResult> {
    Box::pin(async { Ok(EventResult::None) })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Default)]
struct Attempts {
    startup_pending: bool,
    /// Receiver that flips to `true` when the in-flight attempt ends.
    running: Option<(u64, watch::Receiver<bool>)>,
    next_run: u64,
    /// Set after a failed attempt: later turns retry in the background instead of waiting on an
    /// app that may never answer.
    failed: bool,
    failures: u32,
    turns_since_failure: u32,
}

struct SessionDiscovery {
    controller: AbortController,
    attempts: Mutex<Attempts>,
    logger: Option<Arc<dyn maho_ext_api::ComponentLogger>>,
}

impl SessionDiscovery {
    fn log(&self, message: &str, details: &Value) {
        match &self.logger {
            Some(logger) => logger.info(message, Some(details)),
            None => maho_ext_api::diagnostic(&format!("{message} {details}")),
        }
    }

    /// A failed attempt is over before it is reported, so the next turn starts a fresh retry
    /// instead of joining it.
    fn fail_attempt(&self) {
        let mut attempts = lock(&self.attempts);
        attempts.failed = true;
        attempts.failures += 1;
        attempts.turns_since_failure = 0;
        attempts.running = None;
    }

    /// Counts this turn and reports whether the capped backoff allows a background retry on it.
    fn retry_due(&self) -> bool {
        let mut attempts = lock(&self.attempts);
        attempts.turns_since_failure += 1;
        let gap = attempts
            .failures
            .checked_sub(1)
            .and_then(|index| RETRY_GAPS_IN_TURNS.get(index as usize));
        gap.is_some_and(|gap| attempts.turns_since_failure >= *gap)
    }
}

#[derive(Default)]
struct Shared {
    pending: Mutex<BTreeMap<String, oneshot::Sender<Settled>>>,
    sessions: Mutex<BTreeMap<String, Arc<SessionDiscovery>>>,
    grants: Mutex<BTreeMap<String, ToolkitGrant>>,
    registered: Mutex<BTreeSet<String>>,
}

struct Runtime {
    shared: Shared,
    events: maho_ext_api::EventBus,
    offered: BTreeSet<String>,
    timeout_ms: u64,
    /// Registers tools after the registration phase, on a copy of this extension's api.
    late_api: Mutex<ExtensionApi>,
}

fn calling_session_id(context: &ExtensionContext) -> Option<String> {
    let id = context.session_manager.session_id();
    (!id.is_empty()).then(|| id.to_owned())
}

fn settled_value(settled: &Settled) -> Value {
    match settled {
        Ok(grant) => json!({"ok": true, "grant": grant.to_value()}),
        Err(reason) => json!({"ok": false, "reason": reason}),
    }
}

impl Runtime {
    async fn acquire(&self, toolkit: &str, signal: Option<&AbortSignal>) -> Settled {
        if signal.is_some_and(AbortSignal::aborted) {
            return Err("cancelled".into());
        }
        if !self.offered.contains(toolkit) {
            return Err("unavailable".into());
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        let (send, receive) = oneshot::channel();
        lock(&self.shared.pending).insert(request_id.clone(), send);
        self.events.emit(
            "senpi:extension-rpc-event",
            &json!({"name": TOOLKIT_REQUEST_EVENT, "data": {"requestId": request_id, "toolkit": toolkit}}),
        );
        let aborted = async {
            match signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        };
        let settled = tokio::select! {
            received = receive => received.unwrap_or_else(|_| Err("cancelled".into())),
            () = aborted => Err("cancelled".into()),
            () = tokio::time::sleep(Duration::from_millis(self.timeout_ms)) => Err("timeout".into()),
        };
        lock(&self.shared.pending).remove(&request_id);
        settled
    }

    /// Joins the in-flight attempt or starts one. A failed attempt does not wait here; see
    /// `RETRY_GAPS_IN_TURNS`.
    async fn discover(self: &Arc<Self>, session_id: &str, state: &Arc<SessionDiscovery>) {
        if state.controller.signal().aborted() || lock(&self.shared.grants).contains_key(session_id)
        {
            return;
        }
        let mut done = {
            let mut attempts = lock(&state.attempts);
            if let Some((_, receiver)) = &attempts.running {
                receiver.clone()
            } else {
                attempts.startup_pending = false;
                let run = attempts.next_run;
                attempts.next_run += 1;
                let (finished, receiver) = watch::channel(false);
                attempts.running = Some((run, receiver.clone()));
                drop(attempts);
                let runtime = Arc::clone(self);
                let state = Arc::clone(state);
                let session_id = session_id.to_owned();
                tokio::spawn(async move {
                    runtime.run_discovery(&session_id, &state).await;
                    let mut attempts = lock(&state.attempts);
                    if attempts.running.as_ref().is_some_and(|(id, _)| *id == run) {
                        attempts.running = None;
                    }
                    drop(attempts);
                    let _ = finished.send(true);
                });
                receiver
            }
        };
        let _ = done.wait_for(|finished| *finished).await;
    }

    async fn run_discovery(self: &Arc<Self>, session_id: &str, state: &SessionDiscovery) {
        let signal = state.controller.signal();
        let grant = match self.acquire("thread", Some(&signal)).await {
            _ if signal.aborted() => return,
            Err(reason) => {
                state.fail_attempt();
                let reason = if reason == "timeout" {
                    "timeout"
                } else {
                    "grant_refused"
                };
                state.log(
                    "desktop thread toolkit unavailable",
                    &json!({"reason": reason}),
                );
                return;
            }
            Ok(grant) => grant,
        };
        // Remote errors may echo the bearer. Only a fixed diagnostic crosses this boundary.
        let listed = match list_thread_tools(&grant, self.timeout_ms, Some(&signal)).await {
            Ok(listed) => listed,
            Err(_) => {
                if !signal.aborted() {
                    state.fail_attempt();
                    state.log(
                        "desktop thread toolkit unavailable",
                        &json!({"reason": "discovery_failed"}),
                    );
                }
                return;
            }
        };
        for reason in &listed.invalid {
            state.log(
                "desktop thread toolkit skipped tool",
                &json!({"reason": reason}),
            );
        }
        if signal.aborted() {
            return;
        }
        {
            let mut attempts = lock(&state.attempts);
            attempts.failed = false;
            attempts.failures = 0;
        }
        lock(&self.shared.grants).insert(session_id.to_owned(), grant);
        for tool in listed.tools {
            self.register_tool(tool);
        }
    }

    fn register_tool(self: &Arc<Self>, tool: crate::thread_tools::DesktopTool) {
        let name = format!("desktop_{}", tool.name);
        if lock(&self.shared.registered).contains(&name) {
            return;
        }
        let mut api = lock(&self.late_api);
        let inventory = api.get_all_tools().unwrap_or_default();
        if inventory.iter().any(|entry| entry.name == name) {
            return;
        }
        let mut definition = ToolDefinition::new(
            &name,
            tool.description.as_deref().unwrap_or(""),
            tool.input_schema.clone(),
            Arc::new(|_| Box::pin(async { panic!("legacy executor must not run") })),
        );
        definition.label = name.clone();
        let runtime = Arc::clone(self);
        let upstream_name = tool.name.clone();
        let execute: ExtensionToolExecutor =
            Arc::new(move |_id, args, signal, _update, context| {
                let runtime = Arc::clone(&runtime);
                let upstream_name = upstream_name.clone();
                Box::pin(async move {
                    // Never substitute the registering session when the caller has no grant.
                    let grant = calling_session_id(context)
                        .and_then(|caller| lock(&runtime.shared.grants).get(&caller).cloned());
                    Ok(call_thread_tool(
                        grant.as_ref(),
                        &upstream_name,
                        args,
                        runtime.timeout_ms,
                        signal.as_ref(),
                    )
                    .await)
                })
            });
        if api
            .register_tool_with_extension_context(definition, execute)
            .is_ok()
        {
            lock(&self.shared.registered).insert(name);
        }
    }
}

/// Asks the app for a toolkit it hosts for this session's thread; only toolkits listed in
/// `omo_toolkits` are requested.
pub(crate) fn register_runtime(api: &mut ExtensionApi, timeout_ms: u64) {
    let mut tool = ToolDefinition::new(
        DESKTOP_TOOLKIT_TOOL_NAME,
        "Internal channel for skills: ask the app for a toolkit it hosts for this thread, such as its in-app browser. Not for direct use.",
        acquire_parameters(),
        Arc::new(|_| Box::pin(async { panic!("legacy executor must not run") })),
    );
    tool.label = "Desktop toolkit".into();
    tool.exposure = Some(ToolExposure::Eval);
    tool.execution_mode = Some(ToolExecutionMode::Parallel);
    tool.permission_parser = Some(Arc::new(|_, _| Vec::new()));

    // `late_api` is replaced below once the tool and rpc handler exist; the placeholder is never used.
    let placeholder = ExtensionApi::new(
        api.registered.clone(),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    );
    let runtime = Arc::new(Runtime {
        shared: Shared::default(),
        events: api.events.clone(),
        offered: offered_toolkits(&api.profile.session_context),
        timeout_ms,
        late_api: Mutex::new(placeholder),
    });

    let for_tool = Arc::clone(&runtime);
    let execute: ExtensionToolExecutor = Arc::new(move |_id, params, signal, _update, _context| {
        let runtime = Arc::clone(&for_tool);
        Box::pin(async move {
            let toolkit = params.get("toolkit").and_then(Value::as_str).unwrap_or("");
            if params.get("op").and_then(Value::as_str) != Some("acquire") || toolkit.is_empty() {
                return Ok(reply(json!({"ok": false, "reason": "invalid_request"})));
            }
            Ok(reply(settled_value(
                &runtime.acquire(toolkit, signal.as_ref()).await,
            )))
        })
    });
    if let Err(error) = api.register_tool_with_extension_context(tool, execute) {
        std::panic::panic_any(error);
    }

    let for_rpc = Arc::clone(&runtime);
    if let Err(error) = api.rpc_handle(
        TOOLKIT_GRANT_RPC,
        Arc::new(move |message| {
            let runtime = Arc::clone(&for_rpc);
            Box::pin(async move {
                let sender = message
                    .get("requestId")
                    .and_then(Value::as_str)
                    .and_then(|id| lock(&runtime.shared.pending).remove(id));
                let Some(sender) = sender else {
                    return Ok(json!({"accepted": false}));
                };
                let settled = match message.get("grant").and_then(parse_toolkit_grant) {
                    Some(grant) => Ok(grant),
                    None => Err(message
                        .pointer("/refusal/reason")
                        .and_then(Value::as_str)
                        .unwrap_or("invalid_grant")
                        .to_owned()),
                };
                let _ = sender.send(settled);
                Ok(json!({"accepted": true}))
            })
        }),
    ) {
        std::panic::panic_any(error);
    }

    let thread_offered = runtime.offered.contains("thread");
    let for_start = Arc::clone(&runtime);
    api.on(
        EventKind::SessionStart,
        Arc::new(move |_event, context| {
            let runtime = Arc::clone(&for_start);
            if !thread_offered {
                return none();
            }
            let log = |message: &str, details: Value| match &context.logger {
                Some(logger) => logger.info(message, Some(&details)),
                None => maho_ext_api::diagnostic(&format!("{message} {details}")),
            };
            let Some(session_id) = calling_session_id(context) else {
                log(
                    "desktop thread toolkit unavailable",
                    json!({"reason": "missing_session"}),
                );
                return none();
            };
            let state = {
                let mut sessions = lock(&runtime.shared.sessions);
                if sessions.contains_key(&session_id) {
                    return none();
                }
                let state = Arc::new(SessionDiscovery {
                    controller: AbortController::new(),
                    attempts: Mutex::new(Attempts {
                        startup_pending: true,
                        ..Attempts::default()
                    }),
                    logger: context.logger.clone(),
                });
                sessions.insert(session_id.clone(), Arc::clone(&state));
                state
            };
            // Discovery runs after the first paint, off the session-start path; with no host
            // scheduler it starts at once.
            let handle = tokio::runtime::Handle::current();
            let work = move || {
                if !lock(&state.attempts).startup_pending {
                    return;
                }
                drop(handle.spawn(async move {
                    runtime.discover(&session_id, &state).await;
                }));
            };
            match &context.defer_macrotask {
                Some(defer) => defer(Box::new(work)),
                None => work(),
            }
            none()
        }),
    );

    // The first real turn joins the bounded startup attempt. After a failure, retries run in the
    // background without waiting, on the turns `RETRY_GAPS_IN_TURNS` allows, so an app that never
    // answers cannot hold any prompt for the timeout and is not asked on every turn forever. A
    // prompt-cache preview pass must not start a grant request: only the real turn does.
    let for_turn = Arc::clone(&runtime);
    api.on_with_options(
        EventKind::BeforeAgentStart,
        Arc::new(move |event, context| {
            let runtime = Arc::clone(&for_turn);
            let preview =
                matches!(event, ExtensionEvent::BeforeAgentStart(payload) if payload.preview);
            let session_id = calling_session_id(context);
            Box::pin(async move {
                if preview {
                    return Ok(EventResult::None);
                }
                let Some(session_id) = session_id else {
                    return Ok(EventResult::None);
                };
                let state = lock(&runtime.shared.sessions).get(&session_id).cloned();
                let Some(state) = state else {
                    return Ok(EventResult::None);
                };
                if !lock(&state.attempts).failed {
                    runtime.discover(&session_id, &state).await;
                } else if lock(&state.attempts).running.is_none() && state.retry_due() {
                    let background = Arc::clone(&runtime);
                    drop(tokio::spawn(async move {
                        background.discover(&session_id, &state).await;
                    }));
                }
                Ok(EventResult::None)
            })
        }),
        ExtensionHandlerOptions {
            preview_safe: true,
            ..ExtensionHandlerOptions::default()
        },
    );

    let for_shutdown = Arc::clone(&runtime);
    api.on(
        EventKind::SessionShutdown,
        Arc::new(move |_event, context| {
            let runtime = Arc::clone(&for_shutdown);
            if let Some(session_id) = calling_session_id(context) {
                if let Some(state) = lock(&runtime.shared.sessions).remove(&session_id) {
                    state.controller.abort(None);
                }
                lock(&runtime.shared.grants).remove(&session_id);
            }
            none()
        }),
    );

    // Late tool registration publishes this extension's whole tool list, so the copy is taken only
    // after every registration above.
    *lock(&runtime.late_api) = ExtensionApi::new(
        api.registered.clone(),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    );
}
