//! Native registration of the local thread suite, cold wake, control drain, and relay surface.
//!
//! Wires `O01` local thread tools (`thread_create`, `thread_list`, `thread_read`, `thread_send`,
//! `thread_interrupt`, `thread_close`, `thread_handoff`, `thread_rename`, `thread_set_model`,
//! `thread_set_reasoning`, `thread_archive`, `thread_report`, `thread_outbox`, `thread_outbox_ack`,
//! `thread_answer`, plus `thread_bind` & `thread_bindings`) on `ExtensionSide::Brain` with
//! `ToolExposure::Eval`, real Unix-domain JSONL RPC (`LazyLiveThreadSurface`), SQLite store in
//! `ctx.agent_dir`, `InboxDrain` on `SessionControlWake`, `RunContext`, `CompletionTracker`,
//! `initial_skills`, and `monitor` RPC target resolution.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use maho_ext_api::{
    BeforeAgentStartEventResult, ControlEndpointRegisterOptions,
    ControlEndpointRegistrationOutcome, EventKind, EventResult, Extension, ExtensionApi,
    ExtensionContext, ExtensionFailure, ExtensionHandlerOptions, ExtensionRuntime, ExtensionSide,
    SessionControlDisposeFn, ToolCallEventResult, ToolDefinition, ToolExposure, ToolResult,
    diagnostic,
};
use serde_json::{Value, json};

use super::adapter::{
    AdmitExternalMessageInput, DrainWakeEvent, ExternalAdmissionKind, RuntimePhase,
    SessionAdmissionGate, SessionRuntimePort, to_session_control_drain_result,
};
use super::completion::{AgentEndFacts, CompletionTracker};
use super::component::{COMPLETION_SETTLE_WAIT_MS, RunContext};
use super::drain::{InboxDrain, InboxDrainOptions};
use super::endpoint_registry::endpoint_kind_of;
use super::graceful_close_returns::{returned_at_close, settle_graceful_close};
use super::initial_skills::{
    INITIAL_SKILLS_CONTEXT_KEY, INITIAL_SKILLS_ENTRY, names_from_value,
    skill_body_without_frontmatter,
};
use super::lazy_live_surface::LazyLiveThreadSurface;
use super::live_surface_paths::{canonical_socket, resolve_thread_socket};
use super::metadata::THREAD_FAMILY_PROMPT_GUIDELINES;
use super::registration::{drain_event_from_wake, host_instance_of, host_socket_of};
use super::run_alerts::run_alerts::RunAlerts;
use super::same_device_wake::SameDeviceWake;
use super::store::{GatewayStoreOptions, create_gateway_store};
use super::store_api::GatewayStoreApi;
use super::store_relay_ops::{BindInput, BindingsFilter};
use super::tools::gateway_services::create_gateway_services;
use super::tools::monitor_target::{
    MonitorTargetResolution, monitor_rpc_target_from_input, resolve_monitor_target,
};
use super::tools::ports::{ThreadToolSurfaceOptions, UNKNOWN_CALLER};
use super::tools::{BuiltLocalTools, build_local_thread_tools};
use super::types::GatewayDeliveryMode;
use super::wake::host_ensure::{EngineHostEnsureOptions, engine_host_ensure};
use super::wake::runtime::{
    RunningThreadWake, ThreadWakeOptions, start_thread_wake_at_session, take_and_run,
};

pub struct NativeThreadComponent;

struct LiveNativeState {
    agent_dir: PathBuf,
    durable_id: String,
    host_socket: Option<String>,
    store: Arc<dyn GatewayStoreApi>,
    surface: Arc<ThreadToolSurfaceOptions>,
    built: BuiltLocalTools,
    drain: Arc<InboxDrain>,
    run: Arc<Mutex<RunContext>>,
    completions: Arc<CompletionTracker>,
    alerts: Arc<RunAlerts>,
    compacting: Arc<std::sync::atomic::AtomicBool>,
    open_questions: Arc<Mutex<HashSet<String>>>,
    incarnation: String,
    initial_skills: Mutex<Option<Vec<String>>>,
    skill_bodies: Mutex<HashMap<String, String>>,
    warned_skills: Mutex<HashSet<String>>,
    wake: Mutex<Option<Arc<RunningThreadWake>>>,
    wake_cell: Arc<Mutex<Option<Arc<RunningThreadWake>>>>,
    control_dispose: Arc<Mutex<Option<SessionControlDisposeFn>>>,
}

fn native_wake_engine_version() -> String {
    let raw = maho_core::engine_build_identity::ENGINE_VERSION;
    if raw == "0.0.0" || raw.trim().is_empty() {
        "2026.11.19-5".to_owned()
    } else {
        raw.to_owned()
    }
}

impl LiveNativeState {
    fn execute_tool(
        &self,
        name: &str,
        call_id: &str,
        args: Value,
    ) -> super::adapter::BoxFuture<Value> {
        if let Some(tool) = self.built.tools.iter().find(|t| t.name == name) {
            return (tool.execute)(call_id, args);
        }
        let store = self.store.clone();
        let caller_fn = self.surface.caller_session_id.clone();
        let services = create_gateway_services(self.surface.clone());
        match name {
            "thread_bind" => Box::pin(async move {
                let mut input: BindInput = match serde_json::from_value(json!({
                    "platform": args.get("platform").cloned().unwrap_or(Value::Null),
                    "account_id": args.get("account_id").cloned().unwrap_or(Value::Null),
                    "chat_id": args.get("chat_id").cloned().unwrap_or(Value::Null),
                    "thread_id": args.get("thread_id").cloned(),
                    "session_durable_id": caller_fn(),
                    "ttl_seconds": args.get("ttl_seconds").cloned(),
                })) {
                    Ok(i) => i,
                    Err(e) => {
                        return json!({
                            "kind": "error",
                            "error": super::errors::thread_tool_failure(
                                "invalid_arguments",
                                e.to_string(),
                                "Fix the arguments and call again.",
                                None,
                            )
                        });
                    }
                };
                input.ttl_seconds = args.get("ttl_seconds").and_then(Value::as_i64);
                match services.relay.bind(&input) {
                    Ok(binding) => {
                        json!({ "kind": "ok", "binding": binding, "deduplicated": false })
                    }
                    Err(err) => json!({ "kind": "error", "error": err }),
                }
            }),
            "thread_bindings" => Box::pin(async move {
                let filter = BindingsFilter {
                    session_durable_id: Some(caller_fn()),
                    ..Default::default()
                };
                match services.relay.bindings(&filter, None, None) {
                    Ok((bindings, next_cursor)) => {
                        json!({ "kind": "ok", "bindings": bindings, "next_cursor": next_cursor })
                    }
                    Err(err) => json!({ "kind": "error", "error": err }),
                }
            }),
            _ => {
                let _ = store;
                Box::pin(async move {
                    json!({
                        "kind": "error",
                        "error": super::errors::thread_tool_failure(
                            "unsupported",
                            "Unknown thread tool",
                            "Call a supported thread tool.",
                            None,
                        )
                    })
                })
            }
        }
    }
}

fn persisted_initial_skills(entries: &[Value]) -> Option<Vec<String>> {
    for entry in entries.iter().rev() {
        let is_skills = entry.get("type").and_then(Value::as_str) == Some("custom")
            && entry.get("customType").and_then(Value::as_str) == Some(INITIAL_SKILLS_ENTRY);
        if is_skills
            && let Some(names) = entry
                .get("data")
                .and_then(|d| d.get("names"))
                .and_then(names_from_value)
        {
            return Some(names);
        }
    }
    None
}

type NotifyFn = Arc<dyn Fn(&str) + Send + Sync>;

#[allow(clippy::too_many_arguments)]
fn initialize_state(
    agent_dir: PathBuf,
    cwd: PathBuf,
    durable_id: String,
    session_file: Option<String>,
    session_name_fn: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    is_idle_fn: Arc<dyn Fn() -> bool + Send + Sync>,
    runtime: ExtensionRuntime,
    session_context: &BTreeMap<String, String>,
    permission_preset: Option<String>,
    notify_fn: Option<NotifyFn>,
) -> Result<Arc<LiveNativeState>, String> {
    let agent_dir_str = agent_dir.to_string_lossy().into_owned();
    let runtime_instance = host_instance_of(session_context);
    let host_socket = host_socket_of(session_context);

    let mut store_opts = GatewayStoreOptions::new(agent_dir_str);
    store_opts.runtime_instance = runtime_instance.clone();
    let store = create_gateway_store(store_opts).map_err(|e| e.to_string())?;

    let lazy_surface = Arc::new(LazyLiveThreadSurface::new_for_agent(
        agent_dir.clone(),
        host_socket.clone(),
    ));

    let socket_for_ensure = resolve_thread_socket(&agent_dir);
    let socket_for_ensure_closure = socket_for_ensure.clone();
    let agent_dir_ensure = agent_dir.clone();
    let ensure_host: Arc<dyn Fn() -> super::adapter::BoxFuture<Result<(), String>> + Send + Sync> =
        Arc::new(move || {
            let opts = EngineHostEnsureOptions::new(
                socket_for_ensure_closure.clone(),
                agent_dir_ensure.clone(),
                Arc::new(diagnostic),
            );
            let run_ensure = engine_host_ensure(opts);
            Box::pin(async move {
                run_ensure().await;
                Ok(())
            })
        });

    let wake_cell: Arc<Mutex<Option<Arc<RunningThreadWake>>>> = Arc::new(Mutex::new(None));
    let wake_for_carrier = wake_cell.clone();
    let same_device_enabled: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(|_| true);
    let same_device_wake = Arc::new(SameDeviceWake {
        store: store.clone(),
        sessions_directory: agent_dir.join("sessions"),
        enabled: same_device_enabled,
        carrier_wake: Some(Arc::new(move |id| {
            let cell = wake_for_carrier.clone();
            Box::pin(async move {
                let w = cell
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some(wake) = w {
                    wake.wake(&id).await;
                }
            })
        })),
    });

    let run = Arc::new(Mutex::new(RunContext::default()));
    let compacting = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let open_questions = Arc::new(Mutex::new(HashSet::<String>::new()));

    let store_comp = store.clone();
    let store_busy = store.clone();
    let completions = Arc::new(CompletionTracker::new(
        Arc::new(move |did, outcome, through_arm_seq| {
            let st = store_comp.clone();
            Box::pin(async move {
                match st.emit_completions(st.now(), &did, outcome.as_str(), Some(through_arm_seq)) {
                    Ok(items) => Ok(items
                        .into_iter()
                        .map(
                            |(binding_id, cursor)| super::completion::EmittedCompletion {
                                binding_id,
                                cursor,
                            },
                        )
                        .collect()),
                    Err(e) => Err(super::completion::SettleError {
                        is_lock_wait_exceeded: e.is_lock_wait_exceeded(),
                        message: e.message(),
                    }),
                }
            })
        }),
        Arc::new(move || store_busy.busy_timeout_ms()),
        Some(Arc::new(|err, retrying| {
            let prefix = if retrying {
                "thread gateway: completion report not written yet, retrying: "
            } else {
                "thread gateway: completion reports were not written: "
            };
            diagnostic(&format!("{prefix}{err}"));
        })),
    ));

    let caller_id_arc = Arc::new(durable_id.clone());
    let cwd_str = cwd.to_string_lossy().into_owned();
    let cwd_arc = Arc::new(cwd_str.clone());
    let sessions_dir = agent_dir.join("sessions");

    let run_turn = run.clone();
    let run_cause = run.clone();
    let run_hop = run.clone();
    let run_consumed = run.clone();
    let run_reply_to = run.clone();
    let run_reply_senders = run.clone();
    let run_local = run.clone();
    let comp_arm = completions.clone();
    let preset_arc = Arc::new(permission_preset);

    let surface = Arc::new(ThreadToolSurfaceOptions {
        host: lazy_surface,
        store: store.clone(),
        caller_session_id: Arc::new(move || (*caller_id_arc).clone()),
        caller_workspace_root: Arc::new(move || (*cwd_arc).clone()),
        state_directory: agent_dir.clone(),
        disk_sessions: None,
        sessions_directory: Some(Arc::new(move || sessions_dir.clone())),
        ensure_host: Some(ensure_host.clone()),
        same_device_wake: Some(same_device_wake),
        load_provenance_workspaces: Some(Arc::new(|cwd| {
            super::provenance_config::load_provenance_workspaces(cwd, &BTreeMap::new())
        })),
        on_provenance_warning: Some(Arc::new(|msg| {
            diagnostic(&format!("thread gateway: {msg}"));
        })),
        caller_turn_id: Some(Arc::new(move || {
            let t = run_turn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .turn;
            if t == 0 {
                None
            } else {
                Some(format!("turn-{t}"))
            }
        })),
        caller_name: Some(session_name_fn.clone()),
        caller_cause: Some(Arc::new(move || {
            run_cause
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .cause
                .clone()
        })),
        caller_chain_hop: Some(Arc::new(move || {
            run_hop
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .chain_hop
        })),
        caller_run_deliveries: Some(Arc::new(move || {
            run_consumed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .consumed
                .clone()
        })),
        caller_reply_targets: Some(Arc::new(move || {
            run_reply_to
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .reply_to
                .clone()
        })),
        caller_reply_senders: Some(Arc::new(move || {
            run_reply_senders
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .reply_senders
                .clone()
        })),
        caller_run_has_local_input: Some(Arc::new(move || {
            run_local
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .local
        })),
        on_completion_armed: Some(Arc::new(move |did, arm_seq| {
            comp_arm.arm(did, arm_seq);
        })),
        permission_preset: Some(Arc::new(move || (*preset_arc).clone())),
        now: None,
        first_prompt_wait_ms: None,
        remote: None,
        worktree_root: None,
    });

    let built = build_local_thread_tools(surface.clone());

    fn wall_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }
    let alerts = Arc::new(RunAlerts::new(3, 60_000, wall_ms));
    let _ = &built.send_as;
    let _ = &session_name_fn;

    let compacting_p = compacting.clone();
    let oq_p = open_questions.clone();
    let runtime_gate = runtime.clone();
    let runtime_admit = runtime.clone();
    let runtime_ledger = runtime.clone();
    let runtime_port = SessionRuntimePort {
        phase: Arc::new(move || {
            if compacting_p.load(std::sync::atomic::Ordering::Acquire) {
                RuntimePhase::Compacting
            } else if is_idle_fn() {
                RuntimePhase::Idle
            } else if !oq_p
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
            {
                RuntimePhase::WaitingQuestion
            } else {
                RuntimePhase::MidTurn
            }
        }),
        admission_gate: Arc::new(move || {
            let raw = runtime_gate
                .session_actions()
                .map(|a| a.session_control_admission_gate())
                .unwrap_or_else(
                    |_| json!({ "can_admit": true, "editor_revision": 0, "turn_epoch": 0 }),
                );
            SessionAdmissionGate {
                can_admit: raw
                    .get("can_admit")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                hold_reason: raw
                    .get("hold_reason")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                editor_revision: raw
                    .get("editor_revision")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                turn_epoch: raw.get("turn_epoch").and_then(Value::as_i64).unwrap_or(0),
            }
        }),
        admit_external_message: Arc::new(move |input: AdmitExternalMessageInput| {
            let deliver_as = match input.deliver_as {
                GatewayDeliveryMode::Steer => "steer",
                GatewayDeliveryMode::FollowUp | GatewayDeliveryMode::Auto => "followUp",
            };
            let mut payload = json!({
                "delivery_id": input.delivery_id,
                "text": input.text,
                "deliverAs": deliver_as,
                "sender": input.sender,
            });
            if let Some(display_text) = input.display_text {
                payload["display_text"] = Value::String(display_text);
            }
            if let Some(expected_turn_id) = input.expected_turn_id {
                payload["expected_turn_id"] = json!(expected_turn_id);
            }
            let actions = runtime_admit.session_actions().map_err(|e| e.to_string())?;
            let reply = actions.session_control_admit_external_message(payload)?;
            let kind_raw = reply
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("started");
            let kind =
                ExternalAdmissionKind::parse(kind_raw).unwrap_or(ExternalAdmissionKind::Started);
            let turn_epoch = reply.get("turn_epoch").and_then(Value::as_i64).unwrap_or(0);
            Ok(super::adapter::AdmitResult { kind, turn_epoch })
        }),
        list_admitted_deliveries: Arc::new(move || {
            let raw = runtime_ledger
                .session_actions()
                .map(|a| a.session_control_list_admitted_deliveries())
                .unwrap_or_else(|_| json!({ "pending": [], "emitted": [] }));
            serde_json::from_value(raw).unwrap_or_default()
        }),
    };

    let sf_arc = Arc::new(session_file);
    let wake_gate_store = store.clone();
    let did_gate = durable_id.clone();
    let drain = Arc::new(InboxDrain::new(InboxDrainOptions {
        store: store.clone(),
        runtime: runtime_port,
        durable_id: durable_id.clone(),
        session_path: Arc::new(move || (*sf_arc).clone()),
        now: None,
        log: Some(Arc::new(diagnostic)),
        notify: notify_fn,
        on_admitted: None,
        admit_intake: None,
        wake_gated: Some(Arc::new(move || {
            wake_gate_store.woken_gated(&did_gate).unwrap_or(false)
        })),
    }));

    let incarnation = uuid::Uuid::new_v4().to_string();
    let sock_for_reg = host_socket.as_deref().unwrap_or(&socket_for_ensure);
    let kind_for_reg = endpoint_kind_of(sock_for_reg, &[]);
    let _ = store.register_incarnation(
        &durable_id,
        &incarnation,
        host_socket.as_deref(),
        Some(kind_for_reg.as_str()),
    );

    let state = Arc::new(LiveNativeState {
        agent_dir,
        durable_id,
        host_socket: host_socket.clone(),
        store,
        surface,
        built,
        drain,
        run,
        completions,
        alerts,
        compacting,
        open_questions,
        incarnation,
        initial_skills: Mutex::new(None),
        skill_bodies: Mutex::new(HashMap::new()),
        warned_skills: Mutex::new(HashSet::new()),
        wake: Mutex::new(None),
        wake_cell: wake_cell.clone(),
        control_dispose: Arc::new(Mutex::new(None)),
    });

    // If running inside the thread host daemon socket, start wake eagerly.
    if let Some(hs) = host_socket.as_deref()
        && canonical_socket(hs) == canonical_socket(&resolve_thread_socket(&state.agent_dir))
    {
        let ad = state.agent_dir.clone();
        let sess_dir = ad.join("sessions");
        let wake_opts = ThreadWakeOptions {
            engine_version: Some(native_wake_engine_version()),
            ..Default::default()
        };
        if let Some(running) = start_thread_wake_at_session("rpc", ad, sess_dir, wake_opts) {
            *wake_cell
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(running.clone());
            *state
                .wake
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(running);
        }
    }

    Ok(state)
}

fn default_fallback_state(
    runtime: ExtensionRuntime,
    session_context: BTreeMap<String, String>,
) -> Result<Arc<LiveNativeState>, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."));
    let env_map: BTreeMap<String, String> = std::env::vars().collect();
    let agent_dir = maho_omo_agent_home::resolve_agent_home(&env_map, &home, &cwd, |p| p.exists());
    initialize_state(
        agent_dir,
        cwd,
        UNKNOWN_CALLER.to_owned(),
        None,
        Arc::new(|| None),
        Arc::new(|| true),
        runtime,
        &session_context,
        None,
        None,
    )
}

fn state_from_context(
    ctx: &ExtensionContext,
    runtime: &ExtensionRuntime,
    session_context: &BTreeMap<String, String>,
) -> Result<Arc<LiveNativeState>, String> {
    let sm = ctx.session_manager.clone();
    let sm_name = sm.clone();
    let is_idle = ctx.is_idle_fn.clone();
    let ui = ctx.ui.clone();
    let notify: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |text| {
        ui.notify(text, maho_ext_api::NotificationType::Info);
    });
    initialize_state(
        ctx.agent_dir.clone(),
        ctx.cwd.clone(),
        sm.session_id().to_owned(),
        sm.session_file().map(|p| p.to_string_lossy().into_owned()),
        Arc::new(move || sm_name.get_session_name()),
        is_idle,
        runtime.clone(),
        session_context,
        None,
        Some(notify),
    )
}

const TOOL_SPECS: &[(&str, &str)] = &[
    ("thread_create", "Create or open a thread session."),
    ("thread_list", "List active or recent thread sessions."),
    ("thread_read", "Read messages from a thread session."),
    ("thread_send", "Send a message to another thread session."),
    (
        "thread_interrupt",
        "Interrupt a running turn on a thread session.",
    ),
    ("thread_close", "Close a live thread session."),
    (
        "thread_handoff",
        "Hand work to another thread, resolving its address.",
    ),
    ("thread_rename", "Rename a thread session or its alias."),
    (
        "thread_set_model",
        "Switch the active model of a live thread.",
    ),
    (
        "thread_set_reasoning",
        "Switch the thinking level of a live thread.",
    ),
    ("thread_archive", "Archive or unarchive a thread session."),
    (
        "thread_report",
        "Report an event to the conversation bound to this session.",
    ),
    (
        "thread_outbox",
        "Read the rows this session reported to a binding.",
    ),
    (
        "thread_outbox_ack",
        "Acknowledge outbox rows delivered through a binding.",
    ),
    (
        "thread_answer",
        "Answer a question this session asked through a binding.",
    ),
    ("thread_bind", "Bind this session to a conversation thread."),
    (
        "thread_bindings",
        "List this session's conversation bindings.",
    ),
];

impl Extension for NativeThreadComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let state_slot: Arc<Mutex<Option<Arc<LiveNativeState>>>> = Arc::new(Mutex::new(None));
        let runtime = api.runtime.clone();
        let session_context = api.profile.session_context.clone();

        // Register all thread tools directly on `api` with `ExtensionSide::Brain` & `ToolExposure::Eval`.
        for (name, fallback_desc) in TOOL_SPECS {
            let meta = super::metadata::metadata_for(name);
            let description = meta.map(|m| m.description).unwrap_or(*fallback_desc);
            let parameters = super::contracts::params::thread_tool_param_schema(name)
                .unwrap_or_else(|| json!({ "type": "object" }));
            let slot = state_slot.clone();
            let rt = runtime.clone();
            let sc = session_context.clone();
            let tool_name = (*name).to_owned();

            let mut def = ToolDefinition::new(
                name,
                description,
                parameters,
                Arc::new(move |call| {
                    let slot = slot.clone();
                    let rt = rt.clone();
                    let sc = sc.clone();
                    let tname = tool_name.clone();
                    Box::pin(async move {
                        let st = {
                            let mut guard = slot
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            if let Some(existing) = guard.as_ref() {
                                existing.clone()
                            } else {
                                let created = match default_fallback_state(rt, sc) {
                                    Ok(s) => s,
                                    Err(err) => {
                                        let mut r = ToolResult::text(err);
                                        r.is_error = Some(true);
                                        return Ok(r);
                                    }
                                };
                                *guard = Some(created.clone());
                                created
                            }
                        };
                        let result = st.execute_tool(&tname, call.id, call.params).await;
                        let is_error = result.get("kind").and_then(Value::as_str) == Some("error");
                        let mut output = ToolResult::text(result.to_string());
                        output.details = Some(json!({ "result": result }));
                        output.is_error = Some(is_error);
                        Ok(output)
                    })
                }),
            );
            def.exposure = Some(ToolExposure::Eval);
            if *name == "thread_create" {
                def.prompt_guidelines = Some(vec![THREAD_FAMILY_PROMPT_GUIDELINES.to_owned()]);
            }
            if let Err(err) = api.try_register_tool_with_runs(def, Some(ExtensionSide::Brain)) {
                diagnostic(&format!("thread: failed to register tool {name}: {err}"));
            }
        }

        // SessionStart: bind live context, initial skills, wake runtime, and pending completion arms.
        {
            let slot = state_slot.clone();
            let rt = runtime.clone();
            let sc = session_context.clone();
            api.on(
                EventKind::SessionStart,
                Arc::new(move |_, ctx| {
                    let slot = slot.clone();
                    let rt = rt.clone();
                    let sc = sc.clone();
                    Box::pin(async move {
                        let st =
                            state_from_context(ctx, &rt, &sc).map_err(ExtensionFailure::new)?;

                        // Initial skills resolution (`initial-skills.ts`).
                        let entries_json: Vec<Value> = ctx
                            .session_manager
                            .get_entries()
                            .into_iter()
                            .map(|e| {
                                json!({
                                    "id": e.id,
                                    "parentId": e.parent_id,
                                    "timestamp": e.timestamp,
                                    "type": e.kind,
                                    "data": e.data,
                                })
                            })
                            .collect();
                        let persisted = persisted_initial_skills(&entries_json);
                        let from_ctx = sc
                            .get(INITIAL_SKILLS_CONTEXT_KEY)
                            .and_then(|raw| names_from_value(&Value::String(raw.clone())));
                        let chosen = persisted.clone().or(from_ctx.clone());
                        if let Some(names) = chosen {
                            *st.initial_skills
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(names);
                        }
                        if from_ctx.is_some()
                            && persisted.is_none()
                            && let Ok(actions) = rt.message_actions()
                        {
                            let _ = actions.append_entry(
                                INITIAL_SKILLS_ENTRY,
                                Some(json!({ "names": from_ctx })),
                            );
                        }

                        // Pick up pending completion arms from durable store.
                        let did = st.durable_id.clone();
                        if did != UNKNOWN_CALLER
                            && let Ok(Some(arm_seq)) = st.store.latest_completion_arm(&did)
                        {
                            st.completions.arm(&did, arm_seq);
                        }

                        // Initial drain pass + register control endpoint when running on a host/TUI.
                        if let Ok(actions) = rt.session_actions() {
                            let _ = actions.session_control_persist_header_now().await;
                            let drain_for_ep = st.drain.clone();
                            let comp_for_ep = st.completions.clone();
                            let store_for_ep = st.store.clone();
                            let did_for_ep = st.durable_id.clone();
                            let store_for_cb = st.store.clone();
                            let did_for_cb = st.durable_id.clone();
                            let inc_for_cb = st.incarnation.clone();
                            let dispose_slot = st.control_dispose.clone();
                            let inbox_dir = crate::paths::gateway_inbox_directory(
                                &st.agent_dir,
                                &st.durable_id,
                            );
                            let drain_cb: maho_ext_api::SessionControlDrainFn =
                                Arc::new(move |ev| {
                                    let drain = drain_for_ep.clone();
                                    let comp = comp_for_ep.clone();
                                    let store = store_for_ep.clone();
                                    let did = did_for_ep.clone();
                                    Box::pin(async move {
                                        let reason = ev
                                            .get("reason")
                                            .and_then(Value::as_str)
                                            .unwrap_or("inbox");
                                        let delivery_ids: Option<Vec<String>> = ev
                                            .get("delivery_ids")
                                            .and_then(Value::as_array)
                                            .map(|arr| {
                                                arr.iter()
                                                    .filter_map(|v| v.as_str().map(str::to_owned))
                                                    .collect()
                                            });
                                        let wake_ev =
                                            drain_event_from_wake(reason, delivery_ids.as_deref());
                                        if wake_ev.reason == super::adapter::WakeReason::Command
                                            && did != UNKNOWN_CALLER
                                            && let Ok(Some(arm_seq)) =
                                                store.latest_completion_arm(&did)
                                        {
                                            comp.arm(&did, arm_seq);
                                        }
                                        let outcome = drain
                                            .drain_gateway(&wake_ev)
                                            .map_err(|e| e.message())?;
                                        let res = to_session_control_drain_result(&outcome);
                                        Ok(Some(
                                            serde_json::to_value(res)
                                                .unwrap_or_else(|_| json!({ "admitted": [] })),
                                        ))
                                    })
                                });
                            let drain_ref = st.drain.clone();
                            let ref_cb: maho_ext_api::SessionControlReferencedFn =
                                Arc::new(move || {
                                    let d = drain_ref.clone();
                                    Box::pin(async move { d.is_session_referenced() })
                                });
                            let reg_opts = ControlEndpointRegisterOptions {
                                inbox_dir: inbox_dir.to_string_lossy().into_owned(),
                                drain: drain_cb,
                                is_session_referenced: Some(ref_cb),
                            };
                            let on_registered: Arc<
                                dyn Fn(ControlEndpointRegistrationOutcome) + Send + Sync,
                            > = Arc::new(move |outcome| {
                                if let ControlEndpointRegistrationOutcome::Registered {
                                    socket,
                                    dispose,
                                } = outcome
                                {
                                    *dispose_slot
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                        Some(dispose);
                                    let kind = endpoint_kind_of(&socket, &[]);
                                    let _ = store_for_cb.register_incarnation(
                                        &did_for_cb,
                                        &inc_for_cb,
                                        Some(&socket),
                                        Some(kind.as_str()),
                                    );
                                }
                            });
                            if st.host_socket.is_some() {
                                let _ = actions
                                    .defer_control_endpoint_registration(reg_opts, on_registered)
                                    .await;
                            } else {
                                let outcome = actions.register_control_endpoint(reg_opts).await;
                                on_registered(outcome);
                            }
                        }

                        // Initial drain pass.
                        let _ = st.drain.drain(&DrainWakeEvent {
                            reason: super::adapter::WakeReason::Start,
                            delivery_ids: None,
                        });

                        // Start background wake carrier if not already started.
                        let ad = st.agent_dir.clone();
                        let sess_dir = ad.join("sessions");
                        let wake_opts = ThreadWakeOptions {
                            engine_version: Some(native_wake_engine_version()),
                            ..Default::default()
                        };
                        if let Some(running) =
                            start_thread_wake_at_session("rpc", ad, sess_dir, wake_opts)
                        {
                            *st.wake_cell
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(running.clone());
                            *st.wake
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(running);
                        }

                        *slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(st);
                        Ok(EventResult::None)
                    })
                }),
            );
        }

        // BeforeAgentStart: inject initial_skills bodies cleanly (`preview_safe: true`).
        {
            let slot = state_slot.clone();
            api.on_with_options(
                EventKind::BeforeAgentStart,
                Arc::new(move |event, _ctx| {
                    let slot = slot.clone();
                    let maho_ext_api::ExtensionEvent::BeforeAgentStart(bas) = event else {
                        return Box::pin(async { Ok(EventResult::None) });
                    };
                    let sys_prompt = bas.system_prompt.clone();
                    let skills = bas.system_prompt_options.skills.clone();
                    Box::pin(async move {
                        let Some(st) = slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone()
                        else {
                            return Ok(EventResult::None);
                        };
                        let Some(names) = st
                            .initial_skills
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone()
                        else {
                            return Ok(EventResult::None);
                        };
                        let mut blocks = Vec::new();
                        for name in names {
                            let Some(skill) = skills.iter().find(|s| s.name == name) else {
                                warn_initial_skill_once(&st.warned_skills, &name, || {
                                    diagnostic(&format!(
                                        "thread: initial skill {name} is not in this session's skills; it is not applied"
                                    ));
                                });
                                continue;
                            };
                            let cached = st
                                .skill_bodies
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .get(&name)
                                .cloned();
                            let body = match cached {
                                Some(b) => b,
                                None => match skill_body_without_frontmatter(Path::new(&skill.file_path)) {
                                    Ok(b) => {
                                        st.skill_bodies
                                            .lock()
                                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                                            .insert(name.clone(), b.clone());
                                        b
                                    }
                                    Err(err) => {
                                        warn_initial_skill_once(&st.warned_skills, &name, || {
                                            diagnostic(&format!(
                                                "thread: initial skill {name} could not be read: {err}"
                                            ));
                                        });
                                        continue;
                                    }
                                },
                            };
                            blocks.push(format!("<initial_skill name=\"{name}\">\n{body}\n</initial_skill>"));
                        }
                        if blocks.is_empty() {
                            return Ok(EventResult::None);
                        }
                        Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                            system_prompt: Some(format!("{sys_prompt}\n\n{}", blocks.join("\n\n"))),
                            ..Default::default()
                        }))
                    })
                }),
                ExtensionHandlerOptions {
                    preview_safe: true,
                    ..Default::default()
                },
            );
        }

        // SessionControlWake: run inbox drain pass against store.
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::SessionControlWake,
                Arc::new(move |event, _ctx| {
                    let slot = slot.clone();
                    let maho_ext_api::ExtensionEvent::SessionControlWake {
                        reason,
                        delivery_ids,
                        ..
                    } = event
                    else {
                        return Box::pin(async { Ok(EventResult::None) });
                    };
                    let wake_ev = drain_event_from_wake(reason, delivery_ids.as_deref());
                    Box::pin(async move {
                        let Some(st) = slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone()
                        else {
                            return Ok(EventResult::None);
                        };
                        if st
                            .control_dispose
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .is_some()
                        {
                            return Ok(EventResult::None);
                        }
                        if wake_ev.reason == super::adapter::WakeReason::Command
                            && st.durable_id != UNKNOWN_CALLER
                            && let Ok(Some(arm_seq)) =
                                st.store.latest_completion_arm(&st.durable_id)
                        {
                            st.completions.arm(&st.durable_id, arm_seq);
                        }
                        let _ = st.drain.drain(&wake_ev);
                        Ok(EventResult::None)
                    })
                }),
            );
        }

        // Compaction & waiting question phase tracking.
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::SessionBeforeCompact,
                Arc::new(move |_, _| {
                    if let Some(st) = slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                    {
                        st.compacting
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::SessionCompact,
                Arc::new(move |_, _| {
                    if let Some(st) = slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                    {
                        st.compacting
                            .store(false, std::sync::atomic::Ordering::Release);
                    }
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }

        // Turn & message tracking for RunContext, completions, and error-streak alerts.
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::AgentStart,
                Arc::new(move |_, _| {
                    if let Some(st) = slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                    {
                        st.run
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .turn += 1;
                    }
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::MessageStart,
                Arc::new(move |event, _| {
                    if let maho_ext_api::ExtensionEvent::MessageStart { message } = event
                        && let Ok(val) = serde_json::to_value(message)
                        && let Some(st) = slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .as_ref()
                    {
                        st.run
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .note_input(&val);
                    }
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::MessageEnd,
                Arc::new(move |event, _| {
                    if let maho_ext_api::ExtensionEvent::MessageEnd { message } = event
                        && let Ok(val) = serde_json::to_value(message)
                        && let Some(st) = slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .as_ref()
                    {
                        st.run
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .note_assistant_end(&val);
                    }
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::AgentEnd,
                Arc::new(move |event, ctx| {
                    let slot = slot.clone();
                    let did = ctx.session_manager.session_id().to_owned();
                    let (aborted, stop_reason, err_msg) = match event {
                        maho_ext_api::ExtensionEvent::AgentEnd {
                            messages, aborted, ..
                        } => {
                            let last = messages.last().and_then(|m| serde_json::to_value(m).ok());
                            let sr = last
                                .as_ref()
                                .and_then(|v| v.get("stopReason"))
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                            let em = last
                                .as_ref()
                                .and_then(|v| v.get("errorMessage"))
                                .and_then(Value::as_str)
                                .map(str::to_owned);
                            (aborted.unwrap_or(false), sr, em)
                        }
                        _ => (false, None, None),
                    };
                    if let Some(st) = slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_ref()
                    {
                        let facts = AgentEndFacts {
                            aborted,
                            last_assistant_stop_reason: stop_reason,
                            last_error_message: err_msg,
                            provider: None,
                            model: None,
                        };
                        st.completions.agent_end(&did, &facts);
                        st.alerts.agent_end(&did, &facts);
                    }
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::AgentSettled,
                Arc::new(move |_, ctx| {
                    let slot = slot.clone();
                    let did = ctx.session_manager.session_id().to_owned();
                    Box::pin(async move {
                        let Some(st) = slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone()
                        else {
                            return Ok(EventResult::None);
                        };
                        {
                            let mut run = st
                                .run
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            run.consumed.clear();
                            run.local = false;
                            run.answered = true;
                        }
                        let _ = st.alerts.take_settled(&did);
                        let settled_fut = st.completions.settled(&did);
                        let _ = tokio::time::timeout(
                            std::time::Duration::from_millis(COMPLETION_SETTLE_WAIT_MS),
                            settled_fut,
                        )
                        .await;
                        Ok(EventResult::None)
                    })
                }),
            );
        }

        // Monitor tool_call preflight target resolution.
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::ToolCall,
                Arc::new(move |event, ctx| {
                    let slot = slot.clone();
                    let maho_ext_api::ExtensionEvent::ToolCall(tc) = event else {
                        return Box::pin(async { Ok(EventResult::None) });
                    };
                    let Some(raw_target) = monitor_rpc_target_from_input(&tc.tool_name, &tc.input)
                    else {
                        return Box::pin(async { Ok(EventResult::None) });
                    };
                    let caller = ctx.session_manager.session_id().to_owned();
                    let tc_mut_input = &mut tc.input;
                    let resolved_slot = slot.clone();
                    let resolved_target_fut = async move {
                        let state = {
                            resolved_slot
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .clone()
                        };
                        let Some(st) = state else {
                            return MonitorTargetResolution::Keep;
                        };
                        resolve_monitor_target(&st.surface, &raw_target, &caller).await
                    };
                    Box::pin(async move {
                        Ok(apply_monitor_resolution(
                            tc_mut_input,
                            resolved_target_fut.await,
                        ))
                    })
                }),
            );
        }

        // Question blocking tracking.
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::ToolExecutionStart,
                Arc::new(move |event, _| {
                    if let maho_ext_api::ExtensionEvent::ToolExecutionStart {
                        tool_call_id,
                        tool_name,
                        args,
                        ..
                    } = event
                        && let Some(st) = slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .as_ref()
                    {
                        let wait = match tool_name.as_str() {
                            "ask_user_question" => {
                                args.get("waitForAnswer").and_then(Value::as_bool) == Some(true)
                            }
                            "request_user_input" => {
                                args.get("wait_for_answer").and_then(Value::as_bool) == Some(true)
                            }
                            _ => false,
                        };
                        if wait {
                            st.open_questions
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .insert(tool_call_id.clone());
                        }
                    }
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }
        {
            let slot = state_slot.clone();
            api.on(
                EventKind::ToolExecutionEnd,
                Arc::new(move |event, _| {
                    if let maho_ext_api::ExtensionEvent::ToolExecutionEnd { tool_call_id, .. } =
                        event
                        && let Some(st) = slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .as_ref()
                    {
                        st.open_questions
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(tool_call_id);
                    }
                    Box::pin(async { Ok(EventResult::None) })
                }),
            );
        }

        // SessionShutdown: flush completions, dispose surface & settle graceful close.
        {
            let slot = state_slot;
            api.on(
                EventKind::SessionShutdown,
                Arc::new(move |_, ctx| {
                    let slot = slot.clone();
                    let did = ctx.session_manager.session_id().to_owned();
                    let entries: Vec<Value> = ctx
                        .session_manager
                        .get_entries()
                        .into_iter()
                        .map(|e| {
                            json!({
                                "id": e.id,
                                "parentId": e.parent_id,
                                "timestamp": e.timestamp,
                                "type": e.kind,
                                "data": e.data,
                            })
                        })
                        .collect();
                    Box::pin(async move {
                        let Some(st) = slot
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take()
                        else {
                            return Ok(EventResult::None);
                        };
                        st.alerts.ended(&did);
                        st.completions.dispose();
                        let control_dispose = st
                            .control_dispose
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .take();
                        if let Some(dispose) = control_dispose {
                            let _ = dispose().await;
                        }
                        take_and_run(&st.wake, |wake| wake.stop());
                        (st.built.dispose)();
                        let _ = st.store.clear_endpoint(&st.durable_id, &st.incarnation);
                        let returned = returned_at_close(&entries);
                        if !returned.is_empty() {
                            let _ = settle_graceful_close(st.store.as_ref(), &did, &returned).await;
                        }
                        Ok(EventResult::None)
                    })
                }),
            );
        }
    }
}

fn warn_initial_skill_once(warned: &Mutex<HashSet<String>>, name: &str, report: impl FnOnce()) {
    let should_report = warned
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(name.to_owned());
    if should_report {
        report();
    }
}

fn apply_monitor_resolution(input: &mut Value, resolution: MonitorTargetResolution) -> EventResult {
    match resolution {
        MonitorTargetResolution::Refused(fail) => EventResult::ToolCall(ToolCallEventResult {
            block: Some(true),
            reason: Some(format!(
                "{}: {} {}",
                fail.code, fail.message, fail.next_action
            )),
            terminate: None,
        }),
        MonitorTargetResolution::Target(target) => {
            input["target"] = Value::String(target);
            EventResult::None
        }
        MonitorTargetResolution::Keep => EventResult::None,
    }
}

#[cfg(test)]
mod monitor_resolution_tests {
    use super::*;

    #[test]
    fn initial_skill_warning_allows_reentry_and_reports_once() {
        let warned = Mutex::new(HashSet::new());
        warn_initial_skill_once(&warned, "missing", || {
            assert!(warned.try_lock().is_ok(), "warning callback held the mutex");
        });
        warn_initial_skill_once(&warned, "missing", || {
            panic!("the same skill warning must only be reported once");
        });
    }

    #[test]
    fn resolved_monitor_target_reaches_tool_input() {
        let mut input = serde_json::json!({"type":"rpc", "target":"alias"});
        let result = apply_monitor_resolution(
            &mut input,
            MonitorTargetResolution::Target("resolved-id".into()),
        );
        assert!(matches!(result, EventResult::None));
        assert_eq!(input["target"], "resolved-id");
    }
}
