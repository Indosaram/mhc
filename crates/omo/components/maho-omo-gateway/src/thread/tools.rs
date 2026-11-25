//! The thread tools: what a model calls to reach another session or a bound conversation.
//!
//! Upstream counterpart: `thread/tools.ts`. Every failure is data (`{ kind: "error", error }`), and a
//! session op's tool is its own namespace (`ext_<extension>_<toolName>`).

#[path = "tools/archive_ops.rs"]
pub mod archive_ops;
#[path = "tools/gateway_services.rs"]
pub mod gateway_services;
#[path = "tools/internals.rs"]
pub mod internals;
#[path = "tools/local_permission.rs"]
pub mod local_permission;
#[path = "tools/model_match.rs"]
pub mod model_match;
#[path = "tools/monitor_target.rs"]
pub mod monitor_target;
#[path = "tools/ports.rs"]
pub mod ports;
#[path = "tools/read_item.rs"]
pub mod read_item;
#[path = "tools/read_ops.rs"]
pub mod read_ops;
#[path = "tools/relay_tools.rs"]
pub mod relay_tools;
#[path = "tools/reply_permission.rs"]
pub mod reply_permission;
#[path = "tools/reply_target.rs"]
pub mod reply_target;
#[path = "tools/send_view.rs"]
pub mod send_view;
#[path = "tools/thread_row.rs"]
pub mod thread_row;
#[path = "tools/thread_state.rs"]
pub mod thread_state;

pub type ExecuteCallback = Arc<dyn Fn(&str, Value) -> BoxFuture<Value> + Send + Sync>;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::call_budget::call_deadline::{
    TOOL_CALL_BUDGET_MS, wait_within_call_budget, with_call_deadline,
};

use super::adapter::BoxFuture;
use super::address::{ResolveContext, ResolveOptions, ResolveWhere, resolve_address};
use super::address_book::to_thread_address_entries;
use super::constants::GATEWAY_RECEIPT_RETENTION_MS;
use super::contracts::params::{
    ThreadArchiveInput, ThreadCloseInput, ThreadCreateInput, ThreadHandoffInput,
    ThreadInterruptInput, ThreadListInput, ThreadReadInput, ThreadRenameInput, ThreadSendInput,
    ThreadSetModelInput, ThreadSetReasoningInput, thread_tool_param_schema,
};
use super::contracts::parse::parse_thread_params;
use super::contracts::results::ThreadDataError;
use super::control::create::{CreateContext, create_thread};
use super::control::locate::{LocatedControl, locate_control};
use super::control::operations::{
    answered, close_options, close_thread, interrupt_thread, not_live_failure, set_model_on,
    set_thinking_level_on,
};
use super::control::port::control_identity;
use super::control::prompt_start::{
    FIRST_PROMPT_START_RESERVE_MS, FIRST_PROMPT_START_WAIT_MS, await_prompt_start,
};
use super::control::rename::rename_thread;
use super::engine::{GatewayDeliverRequest, GatewayEngine};
use super::errors::{gateway_schema_refusal_message, thread_tool_failure};
use super::pipeline::DeliveryPipeline;
use super::relay::{AnswerRequest, GatewayRelay, Keyed};
use super::store_api::GatewayStoreApi;
use super::store_extensions::{DeclaredSessionOp, SessionCaller, StoreExtensionResult};
use super::store_relay_ops::{
    BindInput, BindingsFilter, ReportOpRequest, ToolReceiptBegin, hash_args,
};
use super::thread_link::thread_link as fmt_thread_link;
use super::transport::{DeliveryTarget, OutboundDelivery};
use super::types::{DeliveryAuthority, GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender};

use gateway_services::create_gateway_services;
use internals::{
    address_book, degraded_summary, failure, host_view, metadata_entry, send_address_book,
    send_book_entries, summary, target_session, uncertain_failure,
};
use ports::{ThreadHostView, ThreadHostViewRequest, ThreadToolSurfaceOptions};
use read_ops::{list_threads, read_thread, with_backlog};
use relay_tools::relay_tool_schemas;
use reply_permission::{ReplySender, consumed_senders_of};
use reply_target::reply_target_mismatch;
use send_view::send_view;
use thread_row::{current_result, thread_row};

pub const RECEIPT_RECOVERY_MAX_KEYS: usize = 4_096;

#[derive(Clone)]
pub struct ThreadTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub execute: ExecuteCallback,
}

impl std::fmt::Debug for ThreadTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ThreadTool")
            .field("name", &self.name)
            .finish()
    }
}

pub struct ThreadToolsOptions {
    pub store: Arc<dyn GatewayStoreApi>,
    pub engine: GatewayEngine,
    pub relay: GatewayRelay,
    /// The calling session's durable id, resolved by the engine, never taken from a tool argument.
    pub caller_session_id: Arc<dyn Fn() -> String + Send + Sync>,
    pub caller_name: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>>,
    /// The caller's own provenance, resolved in its runtime.
    pub caller_provenance: Arc<dyn Fn() -> super::types::Provenance + Send + Sync>,
}

fn error_result(error: impl serde::Serialize) -> Value {
    json!({ "kind": "error", "error": error })
}

fn ok_result(value: Value) -> Value {
    let mut result = value;
    if let Some(object) = result.as_object_mut() {
        object.insert("kind".into(), Value::String("ok".into()));
    }
    result
}

fn invalid_arguments(message: impl Into<String>) -> Value {
    error_result(thread_tool_failure(
        "invalid_arguments",
        message,
        "Fix the arguments and call again.",
        None,
    ))
}

fn delivery_result(result: GatewayDeliveryResult, link: String) -> Value {
    match result {
        GatewayDeliveryResult::Ok {
            thread_id,
            delivery_id,
            message_seq,
            delivery,
            effective_mode,
            endpoint_kind,
            deduplicated,
            collapsed,
        } => {
            let mut val = json!({
                "thread_id": thread_id,
                "thread": thread_id,
                "link": link,
                "delivery": delivery,
                "message_seq": message_seq,
                "deduplicated": deduplicated,
                "delivery_id": delivery_id,
                "effective_mode": effective_mode,
                "endpoint": endpoint_kind.map(|kind| kind.as_str()),
            });
            if collapsed == Some(true)
                && let Some(obj) = val.as_object_mut()
            {
                obj.insert("collapsed".into(), Value::Bool(true));
            }
            ok_result(val)
        }
        GatewayDeliveryResult::Error { error } => error_result(error),
    }
}

fn thread_link(thread_id: &str, title: &str) -> String {
    super::contracts_results::thread_link(thread_id, title)
}

pub type ThreadSendAs = Arc<dyn Fn(String, Value) -> BoxFuture<Value> + Send + Sync>;

pub struct BuiltLocalTools {
    pub tools: Vec<ThreadTool>,
    pub send_as: ThreadSendAs,
    pub dispose: Arc<dyn Fn() + Send + Sync>,
}

struct ReceiptRecoveryState {
    unsettled: HashMap<String, String>,
    unanswered: HashMap<String, i64>,
    admitting: HashMap<String, usize>,
    running: HashSet<String>,
}

impl ReceiptRecoveryState {
    fn new() -> Self {
        Self {
            unsettled: HashMap::new(),
            unanswered: HashMap::new(),
            admitting: HashMap::new(),
            running: HashSet::new(),
        }
    }

    fn held_slots(&self) -> usize {
        let mut held = self.unanswered.len();
        for k in self.admitting.keys() {
            if !self.unanswered.contains_key(k) {
                held += 1;
            }
        }
        held
    }

    fn recoverable(&mut self, key: &str, at: i64) -> bool {
        if let Some(&until) = self.unanswered.get(key)
            && until <= at
        {
            self.unanswered.remove(key);
        }
        self.unanswered.contains_key(key)
    }

    fn recovery_full(&mut self, at: i64) -> bool {
        if self.held_slots() < RECEIPT_RECOVERY_MAX_KEYS {
            return false;
        }
        self.unanswered.retain(|_, until| *until > at);
        self.held_slots() >= RECEIPT_RECOVERY_MAX_KEYS
    }
}

fn receipt_key(principal: &str, operation: &str, idempotency_key: &str) -> String {
    format!("{principal}\0{operation}\0{idempotency_key}")
}

fn map_surface_error(msg: &str) -> Value {
    if let Some(rest) = msg.strip_prefix("host_unavailable:") {
        return failure(
            "host_unavailable",
            format!("The thread host is unavailable at {rest}."),
            "Retry when the shared Senpi host is running.",
            None,
        );
    }
    if let Some(cmd) = msg.strip_prefix("unsupported:") {
        return failure(
            "unsupported",
            format!(
                "The target's endpoint does not accept {cmd}: that terminal runs an engine from before terminal session controls."
            ),
            "Update omo where that terminal runs and restart it, or use thread_send and thread_read (tool.thread_send() and tool.thread_read() in an eval cell); thread_list (tool.thread_list() in an eval cell) shows each thread's controls.",
            Some(json!({ "command": cmd })),
        );
    }
    failure(
        "internal_error",
        format!("Thread operation failed: {msg}"),
        "Call thread_list (tool.thread_list() in an eval cell) and retry after checking the target.",
        None,
    )
}

async fn load_view(
    options: &ThreadToolSurfaceOptions,
    offline: bool,
) -> Result<ThreadHostView, String> {
    if let Some(ensure) = &options.ensure_host {
        let _ = ensure().await;
    }
    host_view(options, ThreadHostViewRequest { offline }).await
}

async fn locate_delivery_target(
    options: &ThreadToolSurfaceOptions,
    current: &ThreadHostView,
    address: &str,
    all_scope: Option<bool>,
    caller_id: &str,
    fuzzy: bool,
) -> Result<DeliveryTarget, Value> {
    let reply_targets = options
        .caller_reply_targets
        .as_ref()
        .map(|f| f())
        .unwrap_or_default();
    let reply_senders = options
        .caller_reply_senders
        .as_ref()
        .map(|f| f())
        .unwrap_or_default();
    let senders: Vec<ReplySender> = if fuzzy {
        Vec::new()
    } else {
        consumed_senders_of(&reply_targets, &reply_senders, address)
    };
    let entries = if fuzzy {
        to_thread_address_entries(&address_book(options, current, &[]))
    } else {
        send_book_entries(options, current, address)
    };
    let is_reply = !senders.is_empty();
    let ctx = ResolveContext {
        remote: if is_reply {
            None
        } else {
            options.remote.clone()
        },
        caller_workspace_root: (options.caller_workspace_root)(),
        local: entries,
        local_for: None,
        caller_id: caller_id.to_owned(),
    };
    let opts = ResolveOptions {
        all_scope: all_scope.unwrap_or(false),
        match_mode: fuzzy.then_some(super::address::types::ResolveMatch::Fuzzy),
        probe: !is_reply,
        id_prefix: if !is_reply {
            super::addressing::IdPrefixMode::Enabled
        } else {
            super::addressing::IdPrefixMode::Disabled
        },
        ..Default::default()
    };
    match resolve_address(&ctx, address, &opts).await {
        Err(err) => Err(json!({ "kind": "error", "error": err })),
        Ok(res) => {
            if let Some(mismatch) =
                reply_target_mismatch(&reply_targets, &reply_senders, address, &res.entry, None)
            {
                return Err(json!({ "kind": "error", "error": mismatch }));
            }
            match res.where_located {
                ResolveWhere::Device { row } => Ok(DeliveryTarget::Remote { row }),
                ResolveWhere::Here { .. } => Ok(DeliveryTarget::Local {
                    thread_id: res.thread,
                    all_scope: res.all_scope.or(all_scope).unwrap_or(false),
                }),
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn deliver_through(
    options: &ThreadToolSurfaceOptions,
    pipeline: &DeliveryPipeline,
    current: &ThreadHostView,
    target: DeliveryTarget,
    thread_msg: &str,
    delivery_mode: Option<&str>,
    expected_turn_id: Option<&str>,
    collapse_key: Option<&str>,
    args_val: Value,
    idempotency_key: &str,
    caller_id: &str,
    resolved_by: Option<&str>,
    authority: Option<DeliveryAuthority>,
) -> Value {
    if caller_id == ports::UNKNOWN_CALLER {
        return failure(
            "caller_context_missing",
            "A gateway send needs the calling session's durable id.",
            "Retry from a session that passes its execution context.",
            None,
        );
    }
    let turn_id = options.caller_turn_id.as_ref().and_then(|f| f());
    let cause_delivery_id = options.caller_cause.as_ref().and_then(|f| f());
    let name = options
        .caller_name
        .as_ref()
        .and_then(|f| f())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    let inbound_hop = options
        .caller_chain_hop
        .as_ref()
        .map(|f| f())
        .filter(|h| *h > 0);
    let mode = delivery_mode
        .and_then(GatewayDeliveryMode::parse)
        .unwrap_or(GatewayDeliveryMode::Auto);

    let sent = pipeline
        .deliver(OutboundDelivery {
            sender: GatewaySender::Session {
                durable_id: caller_id.to_owned(),
                name,
                turn_id,
                cause_delivery_id,
            },
            caller_id: caller_id.to_owned(),
            target,
            text: thread_msg.to_owned(),
            mode,
            expected_turn_id: expected_turn_id.map(str::to_owned),
            idempotency_key: idempotency_key.to_owned(),
            collapse_key: collapse_key.map(str::to_owned),
            client_message_id: None,
            args: args_val,
            inbound_hop,
            authority,
        })
        .await;

    match sent {
        GatewayDeliveryResult::Error { error } => error_result(error),
        GatewayDeliveryResult::Ok {
            thread_id,
            delivery_id,
            message_seq,
            delivery,
            effective_mode,
            deduplicated,
            collapsed,
            ..
        } => {
            if let Some(by) = resolved_by {
                let session = target_session(current, &thread_id);
                let book = send_address_book(options, current, &thread_id, true);
                let entry = book.iter().find(|c| c.thread_id == thread_id);
                let row = match (session, entry) {
                    (Some(s), e) => thread_row(&summary(s, e), None),
                    (None, Some(e)) => thread_row(&degraded_summary(e), None),
                    (None, None) => {
                        return failure(
                            "not_found",
                            format!("Thread {thread_id} is not in the address book."),
                            "Call thread_list (tool.thread_list() in an eval cell) and retry.",
                            None,
                        );
                    }
                };
                let mut map = row.as_object().cloned().unwrap_or_default();
                map.insert("kind".into(), Value::String("ok".into()));
                map.insert("resolved_by".into(), Value::String(by.to_owned()));
                map.insert(
                    "delivery".into(),
                    serde_json::to_value(delivery).unwrap_or(Value::Null),
                );
                map.insert("message_seq".into(), Value::Number(message_seq.into()));
                map.insert("deduplicated".into(), Value::Bool(deduplicated));
                map.insert("delivery_id".into(), Value::String(delivery_id));
                map.insert("effective_mode".into(), Value::String(effective_mode));
                if collapsed == Some(true) {
                    map.insert("collapsed".into(), Value::Bool(true));
                }
                Value::Object(map)
            } else {
                let title = current
                    .sessions
                    .iter()
                    .find(|s| s.durable_session_id.as_deref().unwrap_or(&s.session_id) == thread_id)
                    .and_then(|s| s.name.as_deref());
                let mut obj = json!({
                    "kind": "ok",
                    "thread": thread_id,
                    "link": fmt_thread_link(&thread_id, title),
                    "delivery": delivery,
                    "message_seq": message_seq,
                    "deduplicated": deduplicated,
                    "delivery_id": delivery_id,
                    "effective_mode": effective_mode,
                });
                if collapsed == Some(true)
                    && let Some(map) = obj.as_object_mut()
                {
                    map.insert("collapsed".into(), Value::Bool(true));
                }
                obj
            }
        }
    }
}

/// Builds the complete `O01` local thread tool suite (`tools.ts` + `call_budget` + `control` + `tools/*`).
pub fn build_local_thread_tools(surface: Arc<ThreadToolSurfaceOptions>) -> BuiltLocalTools {
    let services = Arc::new(create_gateway_services(surface.clone()));
    let recovery = Arc::new(Mutex::new(ReceiptRecoveryState::new()));

    let now_fn = surface.now.clone().unwrap_or_else(|| {
        let s = surface.store.clone();
        Arc::new(move || s.now())
    });

    #[allow(clippy::too_many_arguments)]
    async fn run_with_receipt(
        surface: &Arc<ThreadToolSurfaceOptions>,
        recovery: &Mutex<ReceiptRecoveryState>,
        now_fn: &(dyn Fn() -> i64 + Send + Sync),
        name: &str,
        call_id: &str,
        args: &Value,
        receipted: bool,
        offline_view: bool,
        body: impl FnOnce(
            Arc<ThreadToolSurfaceOptions>,
            ThreadHostView,
            String,
            String,
            Option<Option<String>>,
        ) -> BoxFuture<Result<Value, String>>,
    ) -> Value {
        let Some(schema) = thread_tool_param_schema(name) else {
            return invalid_arguments(format!("unknown thread tool schema: {name}"));
        };
        let validated: Value = match parse_thread_params(&schema, args) {
            Ok(v) => v,
            Err(ThreadDataError::Error { error }) => return error_result(error),
        };
        let caller_id = (surface.caller_session_id)();
        let explicit_key = validated
            .get("idempotency_key")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let op_id = explicit_key.unwrap_or_else(|| format!("call:{call_id}"));
        let principal = format!("session:{caller_id}");
        let r_key = receipt_key(&principal, name, &op_id);
        let mut retrying: Option<Option<String>> = None;

        if receipted {
            let at = now_fn();
            {
                let mut st = recovery
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if !st.recoverable(&r_key, at)
                    && !st.admitting.contains_key(&r_key)
                    && st.recovery_full(at)
                {
                    return failure(
                        "overloaded",
                        format!(
                            "{RECEIPT_RECOVERY_MAX_KEYS} earlier calls on this surface failed before the gateway store answered and may still need recovering; nothing ran."
                        ),
                        "Retry those calls under their own idempotency keys, then retry this one.",
                        Some(
                            json!({ "budget": "receipt_recovery", "max_keys": RECEIPT_RECOVERY_MAX_KEYS }),
                        ),
                    );
                }
                *st.admitting.entry(r_key.clone()).or_insert(0) += 1;
            }
            let args_hash = hash_args(&validated);
            let begin_res = surface
                .store
                .tool_receipt_begin(at, &principal, name, &op_id, &args_hash);
            {
                let mut st = recovery
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(c) = st.admitting.get_mut(&r_key) {
                    if *c <= 1 {
                        st.admitting.remove(&r_key);
                    } else {
                        *c -= 1;
                    }
                }
            }
            let admission = match begin_res {
                Ok(a) => a,
                Err(err) => {
                    if err.is_lock_wait_exceeded() {
                        return failure(
                            "overloaded",
                            format!(
                                "The gateway store is locked by another process; nothing ran: {}",
                                err.message()
                            ),
                            "Wait a few seconds, then retry the call.",
                            None,
                        );
                    }
                    if let Some(schema_err) = gateway_schema_refusal_message(&err.message()) {
                        return error_result(schema_err);
                    }
                    recovery
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .unanswered
                        .insert(r_key, at + GATEWAY_RECEIPT_RETENTION_MS);
                    return failure(
                        "internal_error",
                        format!(
                            "The gateway store could not record the call; nothing ran: {}",
                            err.message()
                        ),
                        "Retry the call; if it keeps failing, check the gateway store.",
                        None,
                    );
                }
            };
            let admission = {
                let mut st = recovery
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let rec = st.recoverable(&r_key, at);
                let adjusted = if matches!(admission, ToolReceiptBegin::InProgress)
                    && rec
                    && !st.running.contains(&r_key)
                    && !st.unsettled.contains_key(&r_key)
                {
                    ToolReceiptBegin::Accepted
                } else {
                    admission
                };
                st.unanswered.remove(&r_key);
                adjusted
            };

            match admission {
                ToolReceiptBegin::Replay { result } => return current_result(result),
                ToolReceiptBegin::Conflict => {
                    return failure(
                        "idempotency_conflict",
                        "The idempotency key was already used with different arguments.",
                        "Retry with a new idempotency_key.",
                        None,
                    );
                }
                ToolReceiptBegin::InProgress => {
                    let note = recovery
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .unsettled
                        .get(&r_key)
                        .cloned();
                    if let Some(note) = note {
                        return failure(
                            "idempotency_uncertain",
                            "The earlier operation may have been delivered.",
                            "Read the target transcript before deciding whether to retry.",
                            Some(json!({ "error_note": note })),
                        );
                    }
                    return failure(
                        "idempotency_in_progress",
                        "The same operation is already in progress.",
                        "Wait for the earlier call to settle, then retry.",
                        None,
                    );
                }
                ToolReceiptBegin::Uncertain { error_note } => {
                    if name != "thread_create" {
                        return uncertain_failure(error_note.as_deref());
                    }
                    retrying = Some(error_note);
                }
                ToolReceiptBegin::Accepted => {}
            }
            recovery
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .running
                .insert(r_key.clone());
        }

        let budget_deadline = now_fn() + TOOL_CALL_BUDGET_MS;
        let run_res: Result<Value, String> = with_call_deadline(budget_deadline, async {
            let current = if name == "thread_send"
                && let Some(send_to) = validated.get("thread").and_then(Value::as_str)
                && !send_to.is_empty()
                && send_to != "self"
                && surface
                    .store
                    .session_endpoint(send_to)
                    .ok()
                    .flatten()
                    .is_some()
            {
                send_view(surface, send_to, true).await.unwrap_or_default()
            } else {
                load_view(surface, offline_view).await?
            };
            body(
                surface.clone(),
                current,
                op_id.clone(),
                caller_id.clone(),
                retrying,
            )
            .await
        })
        .await;

        match run_res {
            Ok(val) => {
                if receipted {
                    if let Err(err) = surface.store.tool_receipt_settle(
                        now_fn(),
                        &principal,
                        name,
                        &op_id,
                        Ok(&val),
                    ) {
                        recovery
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .unsettled
                            .insert(
                                r_key.clone(),
                                format!(
                                    "the call ran, and its receipt could not be recorded: {}",
                                    err.message()
                                ),
                            );
                    }
                    recovery
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .running
                        .remove(&r_key);
                }
                val
            }
            Err(err_msg) => {
                if receipted {
                    if let Err(settle_err) = surface.store.tool_receipt_settle(
                        now_fn(),
                        &principal,
                        name,
                        &op_id,
                        Err(&err_msg),
                    ) {
                        recovery
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .unsettled
                            .insert(
                                r_key.clone(),
                                format!(
                                    "the call failed ({err_msg}), and its receipt could not be recorded: {}",
                                    settle_err.message()
                                ),
                            );
                    }
                    recovery
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .running
                        .remove(&r_key);
                }
                if let Some(schema_err) = gateway_schema_refusal_message(&err_msg) {
                    return error_result(schema_err);
                }
                map_surface_error(&err_msg)
            }
        }
    }

    let make_tool = |name: &str, execute: ExecuteCallback| -> ThreadTool {
        let meta = metadata_entry(name);
        ThreadTool {
            name: name.to_owned(),
            description: meta.map(|m| m.description.to_owned()).unwrap_or_default(),
            parameters: thread_tool_param_schema(name)
                .unwrap_or_else(|| json!({ "type": "object" })),
            execute,
        }
    };

    let mut tools = Vec::new();

    // thread_create
    {
        let surf = surface.clone();
        let svcs = services.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_create",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let svcs = svcs.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let parsed_input: ThreadCreateInput = match serde_json::from_value(args.clone())
                    {
                        Ok(v) => v,
                        Err(e) => return invalid_arguments(e.to_string()),
                    };
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_create",
                        &call_id,
                        &args,
                        true,
                        false,
                        move |surf_c, current, op_id, caller_id, retrying| {
                            let svcs_c = svcs.clone();
                            Box::pin(async move {
                                let surf_v = surf_c.clone();
                                let surf_rec = surf_c.clone();
                                let svcs_p = svcs_c.clone();
                                let surf_p = surf_c.clone();
                                let store_w = surf_c.store.clone();
                                let wait_ms = wait_within_call_budget(
                                    surf_c
                                        .first_prompt_wait_ms
                                        .unwrap_or(FIRST_PROMPT_START_WAIT_MS)
                                        as i64,
                                    Some(FIRST_PROMPT_START_RESERVE_MS as i64),
                                    None,
                                )
                                .max(0) as u64;
                                let cid_prompt = caller_id.clone();
                                create_thread(
                                    &surf_c,
                                    &current,
                                    &parsed_input,
                                    CreateContext {
                                        operation_id: op_id,
                                        caller_id,
                                        retrying,
                                        view: Arc::new(move || {
                                            let s = surf_v.clone();
                                            Box::pin(async move { load_view(&s, false).await })
                                        }),
                                        deliver_prompt: Arc::new(
                                            move |prompted, target_id, send, key| {
                                                let sp = surf_p.clone();
                                                let svc = svcs_p.clone();
                                                let cid = cid_prompt.clone();
                                                Box::pin(async move {
                                                    Ok(deliver_through(
                                                        &sp,
                                                        &svc.pipeline,
                                                        &prompted,
                                                        DeliveryTarget::Local {
                                                            thread_id: target_id,
                                                            all_scope: true,
                                                        },
                                                        &send.message,
                                                        send.delivery.as_deref(),
                                                        None,
                                                        None,
                                                        json!({}),
                                                        &key,
                                                        &cid,
                                                        None,
                                                        Some(DeliveryAuthority::Creator),
                                                    )
                                                    .await)
                                                })
                                            },
                                        ),
                                        await_prompt_start: Arc::new(move |did| {
                                            let st = store_w.clone();
                                            Box::pin(async move {
                                                await_prompt_start(st.as_ref(), &did, wait_ms).await
                                            })
                                        }),
                                        record_creator: Arc::new(move |runtime_id, created_val| {
                                            let sr = surf_rec.clone();
                                            let svc = svcs_c.clone();
                                            Box::pin(async move {
                                                let _ = record_thread_creator(
                                                    sr.store.as_ref(),
                                                    &svc.engine.options().resolve,
                                                    &runtime_id,
                                                    &created_val,
                                                )
                                                .await;
                                            })
                                        }),
                                    },
                                )
                                .await
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // thread_list
    {
        let surf = surface.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_list",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let input: ThreadListInput =
                        serde_json::from_value(args.clone()).unwrap_or_default();
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_list",
                        &call_id,
                        &args,
                        true,
                        false,
                        move |surf_c, current, _, _, _| {
                            Box::pin(async move {
                                let listed = list_threads(
                                    &surf_c,
                                    &current,
                                    input.all_scope,
                                    input.state.as_deref(),
                                );
                                Ok(with_backlog(surf_c.store.as_ref(), listed))
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // thread_read
    {
        let surf = surface.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_read",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let input: ThreadReadInput = match serde_json::from_value(args.clone()) {
                        Ok(v) => v,
                        Err(e) => return invalid_arguments(e.to_string()),
                    };
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_read",
                        &call_id,
                        &args,
                        true,
                        false,
                        move |surf_c, current, _, caller_id, _| {
                            Box::pin(async move {
                                answered(|| {
                                    Box::pin(read_thread(&surf_c, &current, &input, &caller_id))
                                })
                                .await
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // thread_send + send_as
    {
        let surf = surface.clone();
        let svcs = services.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_send",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let svcs = svcs.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let input: ThreadSendInput = match serde_json::from_value(args.clone()) {
                        Ok(v) => v,
                        Err(e) => return invalid_arguments(e.to_string()),
                    };
                    let args_c = args.clone();
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_send",
                        &call_id,
                        &args,
                        false,
                        true,
                        move |surf_c, current, op_id, caller_id, _| {
                            let svcs_c = svcs.clone();
                            Box::pin(async move {
                                let target = match locate_delivery_target(
                                    &surf_c,
                                    &current,
                                    &input.thread,
                                    input.all_scope,
                                    &caller_id,
                                    false,
                                )
                                .await
                                {
                                    Ok(t) => t,
                                    Err(err_val) => return Ok(err_val),
                                };
                                Ok(deliver_through(
                                    &surf_c,
                                    &svcs_c.pipeline,
                                    &current,
                                    target,
                                    &input.message,
                                    input.delivery.as_deref(),
                                    input.expected_turn_id.as_deref(),
                                    input.collapse_key.as_deref(),
                                    args_c,
                                    &op_id,
                                    &caller_id,
                                    None,
                                    None,
                                )
                                .await)
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // thread_handoff
    {
        let surf = surface.clone();
        let svcs = services.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_handoff",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let svcs = svcs.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let input: ThreadHandoffInput = match serde_json::from_value(args.clone()) {
                        Ok(v) => v,
                        Err(e) => return invalid_arguments(e.to_string()),
                    };
                    let args_c = args.clone();
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_handoff",
                        &call_id,
                        &args,
                        false,
                        true,
                        move |surf_c, current, op_id, caller_id, _| {
                            let svcs_c = svcs.clone();
                            Box::pin(async move {
                                let fuzzy = input.match_mode.as_deref() == Some("fuzzy");
                                let target = match locate_delivery_target(
                                    &surf_c,
                                    &current,
                                    &input.thread,
                                    input.all_scope,
                                    &caller_id,
                                    fuzzy,
                                )
                                .await
                                {
                                    Ok(t) => t,
                                    Err(err_val) => return Ok(err_val),
                                };
                                Ok(deliver_through(
                                    &surf_c,
                                    &svcs_c.pipeline,
                                    &current,
                                    target,
                                    &input.message,
                                    input.delivery.as_deref(),
                                    input.expected_turn_id.as_deref(),
                                    None,
                                    args_c,
                                    &op_id,
                                    &caller_id,
                                    Some(if fuzzy { "fuzzy" } else { "exact_name" }),
                                    None,
                                )
                                .await)
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // thread_interrupt
    {
        let surf = surface.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_interrupt",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let input: ThreadInterruptInput = match serde_json::from_value(args.clone()) {
                        Ok(v) => v,
                        Err(e) => return invalid_arguments(e.to_string()),
                    };
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_interrupt",
                        &call_id,
                        &args,
                        true,
                        false,
                        move |surf_c, current, _, caller_id, _| {
                            Box::pin(async move {
                                answered(|| {
                                    Box::pin(async move {
                                        match locate_control(
                                            &surf_c,
                                            &current,
                                            &input.thread,
                                            input.all_scope.unwrap_or(false),
                                            &caller_id,
                                        )
                                        .await
                                        {
                                            LocatedControl::Error(err) => Ok(err),
                                            LocatedControl::NotLive { .. } => {
                                                Ok(not_live_failure())
                                            }
                                            LocatedControl::Control(ctrl) => {
                                                interrupt_thread(
                                                    ctrl.as_ref(),
                                                    input.turn_id.as_deref(),
                                                )
                                                .await
                                            }
                                        }
                                    })
                                })
                                .await
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // thread_close
    {
        let surf = surface.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_close",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let input: ThreadCloseInput =
                        serde_json::from_value(args.clone()).unwrap_or_default();
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_close",
                        &call_id,
                        &args,
                        true,
                        false,
                        move |surf_c, current, _, caller_id, _| {
                            Box::pin(async move {
                                answered(|| {
                                    Box::pin(async move {
                                        match locate_control(
                                            &surf_c,
                                            &current,
                                            input.thread.as_deref().unwrap_or("self"),
                                            input.all_scope.unwrap_or(false),
                                            &caller_id,
                                        )
                                        .await
                                        {
                                            LocatedControl::Error(err) => Ok(err),
                                            LocatedControl::NotLive {
                                                thread_id, device, ..
                                            } => {
                                                let mut obj =
                                                    control_identity(&thread_id, device.as_deref())
                                                        .as_object()
                                                        .cloned()
                                                        .unwrap_or_default();
                                                obj.insert(
                                                    "kind".into(),
                                                    Value::String("ok".into()),
                                                );
                                                obj.insert("closed".into(), Value::Bool(false));
                                                obj.insert(
                                                    "reason".into(),
                                                    Value::String("not_live".into()),
                                                );
                                                obj.insert("dropped_deliveries".into(), json!([]));
                                                Ok(Value::Object(obj))
                                            }
                                            LocatedControl::Control(ctrl) => {
                                                close_thread(ctrl.as_ref(), close_options(&input))
                                                    .await
                                            }
                                        }
                                    })
                                })
                                .await
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // thread_rename
    {
        let surf = surface.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_rename",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let input: ThreadRenameInput =
                        serde_json::from_value(args.clone()).unwrap_or_default();
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_rename",
                        &call_id,
                        &args,
                        true,
                        false,
                        move |surf_c, current, op_id, caller_id, _| {
                            Box::pin(async move {
                                answered(|| {
                                    Box::pin(rename_thread(
                                        &surf_c, &current, &input, &op_id, &caller_id,
                                    ))
                                })
                                .await
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // thread_set_model
    {
        let surf = surface.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_set_model",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let input: ThreadSetModelInput = match serde_json::from_value(args.clone()) {
                        Ok(v) => v,
                        Err(e) => return invalid_arguments(e.to_string()),
                    };
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_set_model",
                        &call_id,
                        &args,
                        true,
                        false,
                        move |surf_c, current, _, caller_id, _| {
                            Box::pin(async move {
                                answered(|| {
                                    Box::pin(async move {
                                        match locate_control(
                                            &surf_c,
                                            &current,
                                            &input.thread,
                                            input.all_scope.unwrap_or(false),
                                            &caller_id,
                                        )
                                        .await
                                        {
                                            LocatedControl::Error(err) => Ok(err),
                                            LocatedControl::NotLive { .. } => {
                                                Ok(not_live_failure())
                                            }
                                            LocatedControl::Control(ctrl) => {
                                                set_model_on(
                                                    ctrl.as_ref(),
                                                    &input.model,
                                                    input.provider.as_deref(),
                                                )
                                                .await
                                            }
                                        }
                                    })
                                })
                                .await
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // thread_set_reasoning
    {
        let surf = surface.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_set_reasoning",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let input: ThreadSetReasoningInput = match serde_json::from_value(args.clone())
                    {
                        Ok(v) => v,
                        Err(e) => return invalid_arguments(e.to_string()),
                    };
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_set_reasoning",
                        &call_id,
                        &args,
                        true,
                        false,
                        move |surf_c, current, _, caller_id, _| {
                            Box::pin(async move {
                                answered(|| {
                                    Box::pin(async move {
                                        match locate_control(
                                            &surf_c,
                                            &current,
                                            &input.thread,
                                            input.all_scope.unwrap_or(false),
                                            &caller_id,
                                        )
                                        .await
                                        {
                                            LocatedControl::Error(err) => Ok(err),
                                            LocatedControl::NotLive { .. } => {
                                                Ok(not_live_failure())
                                            }
                                            LocatedControl::Control(ctrl) => {
                                                set_thinking_level_on(
                                                    ctrl.as_ref(),
                                                    &input.level,
                                                    input.scope.as_deref(),
                                                )
                                                .await
                                            }
                                        }
                                    })
                                })
                                .await
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // thread_archive
    {
        let surf = surface.clone();
        let rec = recovery.clone();
        let nf = now_fn.clone();
        tools.push(make_tool(
            "thread_archive",
            Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let rec = rec.clone();
                let nf = nf.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let input: ThreadArchiveInput = match serde_json::from_value(args.clone()) {
                        Ok(v) => v,
                        Err(e) => return invalid_arguments(e.to_string()),
                    };
                    run_with_receipt(
                        &surf,
                        &rec,
                        &*nf,
                        "thread_archive",
                        &call_id,
                        &args,
                        true,
                        false,
                        move |surf_c, current, _, caller_id, _| {
                            Box::pin(async move {
                                answered(|| {
                                    Box::pin(archive_ops::archive_thread(
                                        &surf_c, &current, &input, &caller_id,
                                    ))
                                })
                                .await
                            })
                        },
                    )
                    .await
                })
            }),
        ));
    }

    // Relay tools (thread_report, thread_outbox, thread_outbox_ack, thread_answer)
    for (r_name, r_schema) in relay_tool_schemas() {
        let surf = surface.clone();
        let svcs = services.clone();
        let r_name_owned = r_name.to_owned();
        let meta = metadata_entry(r_name);
        tools.push(ThreadTool {
            name: r_name_owned.clone(),
            description: meta.map(|m| m.description.to_owned()).unwrap_or_default(),
            parameters: r_schema,
            execute: Arc::new(move |call_id, args| {
                let surf = surf.clone();
                let svcs = svcs.clone();
                let r_name = r_name_owned.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let schema = thread_tool_param_schema(&r_name).unwrap_or_else(|| json!({ "type": "object" }));
                    let validated = match parse_thread_params::<Value>(&schema, &args) {
                        Ok(v) => v,
                        Err(ThreadDataError::Error { error }) => return error_result(error),
                    };
                    let caller_id = (surf.caller_session_id)();
                    let _ = call_id;
                    match r_name.as_str() {
                        "thread_report" => {
                            if caller_id == ports::UNKNOWN_CALLER {
                                return failure(
                                    "caller_context_missing",
                                    "A report comes from the calling session, whose durable id is unknown.",
                                    "Retry from a session that passes its execution context.",
                                    None,
                                );
                            }
                            let binding_id = validated.get("binding_id").and_then(Value::as_str).map(str::to_owned);
                            let origins = if binding_id.is_none() {
                                surf.caller_run_deliveries.as_ref().map(|f| f()).unwrap_or_default()
                            } else {
                                Vec::new()
                            };
                            let local_input = binding_id.is_none()
                                && surf.caller_run_has_local_input.as_ref().is_some_and(|f| f());
                            let req = ReportOpRequest {
                                session_durable_id: caller_id.clone(),
                                binding_id,
                                origin_delivery_ids: origins,
                                origin_local_input: local_input,
                                event: validated.get("kind").and_then(Value::as_str).unwrap_or("").to_owned(),
                                text: validated.get("text").and_then(Value::as_str).unwrap_or("").to_owned(),
                                ui_request_id: validated.get("request_id").and_then(Value::as_str).map(str::to_owned),
                                ui_request_kind: validated.get("request_kind").and_then(Value::as_str).map(str::to_owned),
                            };
                            match svcs.relay.report(&req) {
                                Ok(report) => {
                                    if report.armed {
                                        match surf.store.latest_completion_arm(&caller_id).ok().flatten() {
                                            Some(seq) => {
                                                if let Some(cb) = &surf.on_completion_armed {
                                                    cb(&caller_id, seq);
                                                }
                                            }
                                            None => {
                                                return failure(
                                                    "idempotency_uncertain",
                                                    "An earlier completion report under this key was recorded without its arm sequence, and the session has no completion armed now: its completion was already written, or the arm is gone.",
                                                    "Read the binding's outbox with thread_outbox (tool.thread_outbox() in an eval cell) to see whether the completion was sent; to arm another, report again under a new idempotency_key.",
                                                    None,
                                                );
                                            }
                                        }
                                    }
                                    ok_result(json!({
                                        "binding_id": report.binding_id,
                                        "revision": report.revision,
                                        "event": report.event,
                                        "cursor": report.cursor,
                                        "reply_token": report.reply_token,
                                        "armed": report.armed,
                                        "deduplicated": false,
                                    }))
                                }
                                Err(e) => error_result(e),
                            }
                        }
                        "thread_outbox" => {
                            let bid = validated.get("binding_id").and_then(Value::as_str).unwrap_or("");
                            let after = validated.get("after_cursor").and_then(Value::as_i64);
                            let limit = validated.get("limit").and_then(Value::as_i64);
                            match svcs.relay.outbox(bid, after, limit) {
                                Ok(page) => ok_result(json!(page)),
                                Err(e) => error_result(e),
                            }
                        }
                        "thread_outbox_ack" => {
                            let bid = validated.get("binding_id").and_then(Value::as_str).unwrap_or("");
                            let cursor = validated.get("cursor").and_then(Value::as_i64).unwrap_or(0);
                            let pmid = validated.get("provider_message_id").and_then(Value::as_str);
                            match svcs.relay.ack(bid, cursor, pmid) {
                                Ok((acked_cursor, changed)) => ok_result(json!({
                                    "binding_id": bid,
                                    "acked_cursor": acked_cursor,
                                    "changed": changed,
                                })),
                                Err(e) => error_result(e),
                            }
                        }
                        "thread_answer" => {
                            let bid = validated.get("binding_id").and_then(Value::as_str).unwrap_or("").to_owned();
                            let token = validated.get("reply_token").and_then(Value::as_str).unwrap_or("").to_owned();
                            let ans = validated.get("answer").and_then(Value::as_str).unwrap_or("").to_owned();
                            let req = AnswerRequest {
                                binding_id: bid.clone(),
                                reply_token: token,
                                answer: ans,
                                author: None,
                            };
                            match svcs.relay.answer(&req).await {
                                Ok((claim, answered_by)) => ok_result(json!({
                                    "binding_id": bid,
                                    "cursor": claim.cursor,
                                    "session_durable_id": claim.session_durable_id,
                                    "answered_by": answered_by,
                                })),
                                Err(e) => error_result(e),
                            }
                        }
                        _ => invalid_arguments("unsupported relay tool"),
                    }
                })
            }),
        });
    }

    let send_as: ThreadSendAs = {
        let surf = surface.clone();
        let svcs = services.clone();
        let rec = recovery.clone();
        let nf = now_fn;
        Arc::new(move |caller_id, args| {
            let surf = surf.clone();
            let svcs = svcs.clone();
            let rec = rec.clone();
            let nf = nf.clone();
            Box::pin(async move {
                let input: ThreadSendInput = match serde_json::from_value(args.clone()) {
                    Ok(v) => v,
                    Err(e) => return invalid_arguments(e.to_string()),
                };
                let key = input
                    .idempotency_key
                    .clone()
                    .unwrap_or_else(|| format!("send-as:{caller_id}"));
                let _ = caller_id;
                let args_c = args.clone();
                run_with_receipt(
                    &surf,
                    &rec,
                    &*nf,
                    "thread_send",
                    &key,
                    &args,
                    false,
                    true,
                    move |surf_c, current, op_id, cid, _| {
                        let svcs_c = svcs.clone();
                        Box::pin(async move {
                            let target = match locate_delivery_target(
                                &surf_c,
                                &current,
                                &input.thread,
                                input.all_scope,
                                &cid,
                                false,
                            )
                            .await
                            {
                                Ok(t) => t,
                                Err(err_val) => return Ok(err_val),
                            };
                            Ok(deliver_through(
                                &surf_c,
                                &svcs_c.pipeline,
                                &current,
                                target,
                                &input.message,
                                input.delivery.as_deref(),
                                input.expected_turn_id.as_deref(),
                                input.collapse_key.as_deref(),
                                args_c,
                                &op_id,
                                &cid,
                                None,
                                None,
                            )
                            .await)
                        })
                    },
                )
                .await
            })
        })
    };

    let svcs_d = services;
    let rec_d = recovery;
    let dispose = Arc::new(move || {
        svcs_d.pipeline.dispose();
        rec_d
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unanswered
            .clear();
    });

    BuiltLocalTools {
        tools,
        send_as,
        dispose,
    }
}

/// Every thread tool this component owns.
pub fn create_thread_tools(options: Arc<ThreadToolsOptions>) -> Vec<ThreadTool> {
    let mut tools = Vec::new();

    for (name, description) in [
        (
            "thread_send",
            "Send a message to another thread and report how it was delivered.",
        ),
        (
            "thread_handoff",
            "Hand work to another thread, resolving its address first.",
        ),
    ] {
        let options = options.clone();
        let tool_name = name.to_owned();
        tools.push(ThreadTool {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "thread": { "type": "string", "minLength": 1 },
                    "message": { "type": "string", "minLength": 1 },
                    "delivery": { "type": "string", "enum": ["auto", "steer", "follow_up"] },
                    "expected_turn_id": { "type": "integer" },
                },
                "required": ["thread", "message"],
                "additionalProperties": false,
            }),
            execute: Arc::new(move |call_id: &str, args: Value| {
                let options = options.clone();
                let tool_name = tool_name.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let Some(thread) = args.get("thread").and_then(Value::as_str) else {
                        return invalid_arguments("thread must be a non-empty string");
                    };
                    let Some(message) = args.get("message").and_then(Value::as_str) else {
                        return invalid_arguments("message must be a non-empty string");
                    };
                    let mode = match args.get("delivery").and_then(Value::as_str) {
                        Some(mode) => match GatewayDeliveryMode::parse(mode) {
                            Some(mode) => Some(mode),
                            None => {
                                return invalid_arguments(
                                    "delivery must be auto, steer or follow_up",
                                );
                            }
                        },
                        None => None,
                    };
                    let sender = GatewaySender::Session {
                        durable_id: (options.caller_session_id)(),
                        name: options.caller_name.as_ref().and_then(|name| name()),
                        turn_id: None,
                        cause_delivery_id: None,
                    };
                    let provenance = (options.caller_provenance)();
                    let provenance = Arc::new(provenance);
                    let engine = GatewayEngine::new(super::engine::GatewayEngineOptions {
                        sender_provenance: Some(Arc::new(move || (*provenance).clone())),
                        store: options.store.clone(),
                        endpoints: options.engine.options().endpoints.clone(),
                        resolve: options.engine.options().resolve.clone(),
                        now: options.engine.options().now.clone(),
                    });
                    let mut request = GatewayDeliverRequest::new(sender, thread, message);
                    request.mode = mode;
                    request.expected_turn_id = args.get("expected_turn_id").and_then(Value::as_i64);
                    request.idempotency_key = Some(call_id.clone());
                    let result = engine.deliver(request).await;
                    let _ = &tool_name;
                    delivery_result(result, thread_link(thread, thread))
                })
            }),
        });
    }

    {
        let options = options.clone();
        tools.push(ThreadTool {
            name: "thread_report".into(),
            description: "Report an event to the conversation bound to this session.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "event": { "type": "string", "enum": ["milestone", "report", "question", "completion"] },
                    "text": { "type": "string", "minLength": 1 },
                    "binding_id": { "type": "string" },
                    "request_id": { "type": "string" },
                    "request_kind": { "type": "string", "enum": ["question", "select", "confirm", "input", "editor"] },
                },
                "required": ["event", "text"],
                "additionalProperties": false,
            }),
            execute: Arc::new(move |call_id: &str, args: Value| {
                let options = options.clone();
                let call_id = call_id.to_owned();
                Box::pin(async move {
                    let (Some(event), Some(text)) = (
                        args.get("event").and_then(Value::as_str),
                        args.get("text").and_then(Value::as_str),
                    ) else {
                        return invalid_arguments("event and text are required");
                    };
                    let request = ReportOpRequest {
                        session_durable_id: (options.caller_session_id)(),
                        binding_id: args.get("binding_id").and_then(Value::as_str).map(str::to_owned),
                        origin_delivery_ids: Vec::new(),
                        origin_local_input: false,
                        event: event.to_owned(),
                        text: text.to_owned(),
                        ui_request_id: args.get("request_id").and_then(Value::as_str).map(str::to_owned),
                        ui_request_kind: args.get("request_kind").and_then(Value::as_str).map(str::to_owned),
                    };
                    let _ = call_id;
                    match options.relay.report(&request) {
                        Ok(report) => ok_result(json!({
                            "binding_id": report.binding_id,
                            "revision": report.revision,
                            "event": report.event,
                            "cursor": report.cursor,
                            "reply_token": report.reply_token,
                            "armed": report.armed,
                            "deduplicated": false,
                        })),
                        Err(error) => error_result(error),
                    }
                })
            }),
        });
    }

    {
        let options = options.clone();
        tools.push(ThreadTool {
            name: "thread_bind".into(),
            description: "Bind this session to a conversation thread.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "platform": { "type": "string" },
                    "account_id": { "type": "string" },
                    "chat_id": { "type": "string" },
                    "thread_id": { "type": "string" },
                    "ttl_seconds": { "type": "integer" },
                },
                "required": ["platform", "account_id", "chat_id"],
                "additionalProperties": false,
            }),
            execute: Arc::new(move |_call_id: &str, args: Value| {
                let options = options.clone();
                Box::pin(async move {
                    let mut input: BindInput = match serde_json::from_value(json!({
                        "platform": args.get("platform").cloned().unwrap_or(Value::Null),
                        "account_id": args.get("account_id").cloned().unwrap_or(Value::Null),
                        "chat_id": args.get("chat_id").cloned().unwrap_or(Value::Null),
                        "thread_id": args.get("thread_id").cloned(),
                        "session_durable_id": (options.caller_session_id)(),
                        "ttl_seconds": args.get("ttl_seconds").cloned(),
                    })) {
                        Ok(input) => input,
                        Err(error) => return invalid_arguments(error.to_string()),
                    };
                    input.ttl_seconds = args.get("ttl_seconds").and_then(Value::as_i64);
                    match options.relay.bind(&input) {
                        Ok(binding) => {
                            ok_result(json!({ "binding": binding, "deduplicated": false }))
                        }
                        Err(error) => error_result(error),
                    }
                })
            }),
        });
    }

    {
        let options = options.clone();
        tools.push(ThreadTool {
            name: "thread_bindings".into(),
            description: "List this session's conversation bindings.".into(),
            parameters: json!({ "type": "object", "properties": {}, "additionalProperties": false }),
            execute: Arc::new(move |_call_id: &str, _args: Value| {
                let options = options.clone();
                Box::pin(async move {
                    let filter = BindingsFilter {
                        session_durable_id: Some((options.caller_session_id)()),
                        ..Default::default()
                    };
                    match options.relay.bindings(&filter, None, None) {
                        Ok((bindings, next_cursor)) => {
                            ok_result(json!({ "bindings": bindings, "next_cursor": next_cursor }))
                        }
                        Err(error) => error_result(error),
                    }
                })
            }),
        });
    }

    {
        let options = options.clone();
        tools.push(ThreadTool {
            name: "thread_outbox".into(),
            description: "Read the rows this session reported to a binding.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "binding_id": { "type": "string" },
                    "after_cursor": { "type": "integer" },
                    "limit": { "type": "integer" }
                },
                "required": ["binding_id"],
                "additionalProperties": false,
            }),
            execute: Arc::new(move |_call_id: &str, args: Value| {
                let options = options.clone();
                Box::pin(async move {
                    let Some(binding_id) = args.get("binding_id").and_then(Value::as_str) else {
                        return invalid_arguments("binding_id is required");
                    };
                    match options.relay.outbox(
                        binding_id,
                        args.get("after_cursor").and_then(Value::as_i64),
                        args.get("limit").and_then(Value::as_i64),
                    ) {
                        Ok(page) => ok_result(json!(page)),
                        Err(error) => error_result(error),
                    }
                })
            }),
        });
    }

    {
        let options = options.clone();
        tools.push(ThreadTool {
            name: "thread_answer".into(),
            description: "Answer a question this session asked through a binding.".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "binding_id": { "type": "string" },
                    "reply_token": { "type": "string" },
                    "answer": { "type": "string" }
                },
                "required": ["binding_id", "reply_token", "answer"],
                "additionalProperties": false,
            }),
            execute: Arc::new(move |_call_id: &str, args: Value| {
                let options = options.clone();
                Box::pin(async move {
                    let (Some(binding_id), Some(reply_token), Some(answer)) = (
                        args.get("binding_id").and_then(Value::as_str),
                        args.get("reply_token").and_then(Value::as_str),
                        args.get("answer").and_then(Value::as_str),
                    ) else {
                        return invalid_arguments(
                            "binding_id, reply_token and answer are required",
                        );
                    };
                    let request = AnswerRequest {
                        binding_id: binding_id.to_owned(),
                        reply_token: reply_token.to_owned(),
                        answer: answer.to_owned(),
                        author: None,
                    };
                    match options.relay.answer(&request).await {
                        Ok((claim, answered_by)) => ok_result(json!({
                            "binding_id": binding_id,
                            "cursor": claim.cursor,
                            "session_durable_id": claim.session_durable_id,
                            "answered_by": answered_by,
                        })),
                        Err(error) => error_result(error),
                    }
                })
            }),
        });
    }

    tools
}

pub type CallerResolver = Arc<dyn Fn(String) -> BoxFuture<Option<String>> + Send + Sync>;

pub async fn resolve_caller_durable_id(
    resolve: &super::engine::GatewayResolve,
    runtime_id: &str,
) -> Option<String> {
    if runtime_id.is_empty() || runtime_id == "unknown" || runtime_id == ports::UNKNOWN_CALLER {
        return None;
    }
    match resolve(runtime_id, true).await {
        super::engine::GatewayResolution::Ok(target) if target.durable_id == runtime_id => {
            Some(target.durable_id)
        }
        _ => None,
    }
}

pub fn register_extension_tools(
    store: Arc<dyn GatewayStoreApi>,
    caller: Arc<dyn Fn() -> String + Send + Sync>,
) -> Vec<ThreadTool> {
    build_extension_tools(store, caller, Arc::new(|_| Box::pin(async { None })))
}

pub fn build_extension_tools(
    store: Arc<dyn GatewayStoreApi>,
    caller: Arc<dyn Fn() -> String + Send + Sync>,
    resolve: CallerResolver,
) -> Vec<ThreadTool> {
    store
        .session_callable_ops()
        .into_iter()
        .map(|declared| extension_tool(store.clone(), caller.clone(), resolve.clone(), declared))
        .collect()
}

fn extension_tool(
    store: Arc<dyn GatewayStoreApi>,
    caller: Arc<dyn Fn() -> String + Send + Sync>,
    resolve: CallerResolver,
    declared: DeclaredSessionOp,
) -> ThreadTool {
    ThreadTool {
        name: declared.registered_name,
        description: declared.entry.description.unwrap_or_default(),
        parameters: declared.entry.parameters,
        execute: Arc::new(move |_call_id: &str, args: Value| {
            let (store, caller, resolve, extension, op) = (
                store.clone(),
                caller.clone(),
                resolve.clone(),
                declared.extension.clone(),
                declared.entry.op.clone(),
            );
            Box::pin(async move {
                let result = match resolve(caller()).await {
                    Some(caller_durable_id) => store.extension_session_await(
                        &extension,
                        &op,
                        &args,
                        &SessionCaller { caller_durable_id },
                        store.now(),
                    ),
                    None => StoreExtensionResult::refused(
                        super::store_extensions::StoreExtensionRefusalCode::CallerContextMissing,
                        "The calling session's durable id is not in the thread address book; the op was not run.",
                    ),
                };
                match result {
                    StoreExtensionResult::Ok { value } => json!({"kind":"ok", "value":value}),
                    StoreExtensionResult::Refused { code, message } => {
                        json!({"kind":"refused", "code":code.as_str(), "message":message})
                    }
                }
            })
        }),
    }
}

pub async fn record_thread_creator(
    store: &dyn GatewayStoreApi,
    resolve: &super::engine::GatewayResolve,
    runtime_id: &str,
    created: &Value,
) -> Result<(), super::store_ops::StoreError> {
    if created["kind"] != "ok" {
        return Ok(());
    }
    let Some(created_id) = created["thread"]
        .as_str()
        .or_else(|| created["thread"]["thread_id"].as_str())
    else {
        return Ok(());
    };
    if let Some(creator) = resolve_caller_durable_id(resolve, runtime_id).await
        && creator != created_id
    {
        store.record_thread_creation(
            &super::store_extension_facade::ThreadCreationRecord {
                creator_durable_id: creator,
                created_durable_id: created_id.to_owned(),
            },
            store.now(),
        )?;
    }
    Ok(())
}

#[derive(Default)]
pub struct ExtensionToolRegistration {
    mine: HashSet<String>,
    reported: HashSet<String>,
}

impl ExtensionToolRegistration {
    pub fn session_start(
        &mut self,
        tools: Vec<ThreadTool>,
        present: &[String],
        log: &dyn Fn(&str),
    ) -> Vec<ThreadTool> {
        let mut counts = HashMap::new();
        for tool in &tools {
            *counts.entry(tool.name.clone()).or_insert(0usize) += 1;
        }
        tools
            .into_iter()
            .filter(|tool| {
                let why = if counts[&tool.name] > 1 {
                    Some("more than one extension declares it")
                } else if present.contains(&tool.name) && !self.mine.contains(&tool.name) {
                    Some("the session already has a tool by that name")
                } else {
                    None
                };
                if let Some(why) = why {
                    if self.reported.insert(tool.name.clone()) {
                        log(&format!(
                            "thread gateway: extension session tool {} was not registered: {why}",
                            tool.name
                        ));
                    }
                    false
                } else {
                    self.mine.insert(tool.name.clone());
                    true
                }
            })
            .collect()
    }
}

/// The key a tool call's receipt is recorded under.
pub fn call_key(principal: &str, call_id: &str) -> Keyed {
    Keyed {
        principal: principal.to_owned(),
        idempotency_key: Some(call_id.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn caller_identity_must_equal_address_book_durable_id() {
        let resolver: super::super::engine::GatewayResolve = Arc::new(|_, _| {
            Box::pin(async {
                super::super::engine::GatewayResolution::Ok(super::super::engine::GatewayTarget {
                    durable_id: "dur-1".into(),
                    endpoint: None,
                    liveness: super::super::adapter::EndpointLiveness::Dead,
                })
            })
        });
        assert_eq!(
            resolve_caller_durable_id(&resolver, "dur-1")
                .await
                .as_deref(),
            Some("dur-1")
        );
        assert_eq!(resolve_caller_durable_id(&resolver, "rpc-1").await, None);
    }
}
