//! The gateway SDK: sends and reports over one store, for a CLI or a connector.
//!
//! Upstream counterpart: `thread/sdk.ts`. A send resolves sender provenance in the sender's own
//! runtime and reports whether an armed completion wake reached its target.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{Value, json};
use uuid::Uuid;

use crate::call_budget::call_deadline::{
    TOOL_CALL_BUDGET_MS, wait_within_call_budget, with_call_deadline,
};

use super::adapter::BoxFuture;
use super::address::{ResolveContext, ResolveOptions, ResolveWhere, resolve_address};
use super::address_book::to_thread_address_entries;
use super::contracts::params::{
    ThreadCloseInput, ThreadCreateInput, ThreadInterruptInput, ThreadListInput, ThreadReadInput,
    ThreadRenameInput, ThreadSetModelInput, thread_tool_param_schema,
};
use super::contracts::parse::parse_thread_params;
use super::contracts::results::ThreadDataError;
use super::control::create::{CreateContext, create_thread};
use super::control::locate::{LocatedControl, locate_control};
use super::control::operations::{
    answered, close_options, close_thread, interrupt_thread, not_live_failure, set_model_on,
};
use super::control::port::control_identity;
use super::control::prompt_start::{
    FIRST_PROMPT_START_RESERVE_MS, FIRST_PROMPT_START_WAIT_MS, await_prompt_start,
};
use super::control::rename::rename_thread;
use super::engine::{GatewayDeliverRequest, GatewayEngine};
use super::errors::{ThreadToolFailure, thread_tool_failure};
use super::live_surface::LiveThreadSurface;
use super::live_surface_paths::resolve_thread_socket;
use super::provenance_resolver::{ProvenanceRuntime, ProvenanceWorkspace, resolve_provenance};
use super::relay::{GatewayRelay, RelayResult};
use super::store_api::GatewayStoreApi;
use super::store_extensions::{
    DeclaredSessionOp, SessionCaller, StoreExtensionRegistration, StoreExtensionResult,
};
use super::store_relay_ops::{
    BindInput, BindingRecord, RelayOutcome, ReportOpRequest, ReportOpResult,
};
use super::tools::internals::{address_book, host_view};
use super::tools::ports::{
    ThreadHost, ThreadHostViewRequest, ThreadToolSurfaceOptions, UNKNOWN_CALLER,
};
use super::tools::read_ops::{list_threads_async, read_thread, with_backlog};
use super::types::{GatewayDeliveryMode, GatewayDeliveryResult, GatewaySender, Provenance};
use super::wake::host_ensure::{EngineHostEnsureOptions, engine_host_ensure};

pub struct ThreadSdkOptions {
    pub agent_dir: String,
    /** The caller's working directory: the workspace a call without `all_scope` is scoped to. */
    pub cwd: String,
    pub env: BTreeMap<String, String>,
    pub uid: i64,
    pub user: String,
    pub store: Arc<dyn GatewayStoreApi>,
    pub engine: GatewayEngine,
    pub relay: GatewayRelay,
    pub workspaces: Option<Vec<ProvenanceWorkspace>>,
}

/// Validates minimum required SDK configuration fields (`assertThreadSdkOptions`).
pub fn assert_thread_sdk_options(agent_dir: &str, cwd: &str, user: &str) -> Result<(), String> {
    let mut missing = Vec::new();
    if agent_dir.is_empty() {
        missing.push("agentDir");
    }
    if cwd.is_empty() {
        missing.push("cwd");
    }
    if user.is_empty() {
        missing.push("user");
    }
    if !missing.is_empty() {
        return Err(format!(
            "createThreadSdk needs {{ agentDir, cwd, uid, user }}; missing or invalid: {}",
            missing.join(", ")
        ));
    }
    Ok(())
}

/// The sender's own provenance, resolved once per send in the sender's runtime.
pub fn sdk_provenance(
    cwd: &str,
    env: &BTreeMap<String, String>,
    workspaces: Option<Vec<ProvenanceWorkspace>>,
) -> Provenance {
    let mut runtime = ProvenanceRuntime {
        cwd: std::path::PathBuf::from(cwd),
        env: env.clone(),
        workspaces,
        ..Default::default()
    };
    if runtime.env.is_empty() {
        runtime.env = std::env::vars().collect();
    }
    resolve_provenance(&runtime)
}

#[derive(Clone)]
pub struct ThreadSdk {
    options: Arc<ThreadSdkOptions>,
    host: Option<Arc<dyn ThreadHost>>,
}

impl ThreadSdk {
    pub fn new(options: ThreadSdkOptions) -> Self {
        Self {
            options: Arc::new(options),
            host: None,
        }
    }

    pub fn with_host(options: ThreadSdkOptions, host: Arc<dyn ThreadHost>) -> Self {
        Self {
            options: Arc::new(options),
            host: Some(host),
        }
    }

    pub fn principal(&self) -> String {
        format!("cli:{}", self.options.uid)
    }

    fn sender(&self) -> GatewaySender {
        GatewaySender::Cli {
            uid: self.options.uid,
            user: self.options.user.clone(),
        }
    }

    fn surface_options(&self) -> ThreadToolSurfaceOptions {
        let agent_dir = self.options.agent_dir.clone();
        let cwd = self.options.cwd.clone();
        let workspaces = self.options.workspaces.clone();
        let host: Arc<dyn ThreadHost> = self.host.clone().unwrap_or_else(|| {
            Arc::new(LiveThreadSurface::new_real(PathBuf::from(&agent_dir), None))
        });
        let sessions_dir = PathBuf::from(&agent_dir).join("sessions");
        let agent_dir_for_ensure = PathBuf::from(&agent_dir);
        let socket_for_ensure = resolve_thread_socket(&agent_dir_for_ensure);
        ThreadToolSurfaceOptions {
            host,
            store: self.options.store.clone(),
            caller_session_id: Arc::new(|| UNKNOWN_CALLER.to_owned()),
            caller_workspace_root: Arc::new(move || cwd.clone()),
            state_directory: PathBuf::from(&agent_dir),
            disk_sessions: None,
            sessions_directory: Some(Arc::new(move || sessions_dir.clone())),
            ensure_host: Some(Arc::new(move || {
                let opts = EngineHostEnsureOptions::new(
                    socket_for_ensure.clone(),
                    agent_dir_for_ensure.clone(),
                    Arc::new(|_| {}),
                );
                let run_ensure = engine_host_ensure(opts);
                Box::pin(async move {
                    run_ensure().await;
                    Ok(())
                })
            })),
            same_device_wake: None,
            load_provenance_workspaces: workspaces.map(
                |w| -> super::tools::ports::ProvenanceWorkspacesLoader {
                    Arc::new(move |_| w.clone())
                },
            ),
            on_provenance_warning: None,
            caller_turn_id: None,
            caller_name: None,
            caller_cause: None,
            caller_chain_hop: None,
            caller_run_deliveries: None,
            caller_reply_targets: None,
            caller_reply_senders: None,
            caller_run_has_local_input: None,
            on_completion_armed: None,
            permission_preset: None,
            now: None,
            first_prompt_wait_ms: None,
            remote: None,
            worktree_root: None,
        }
    }

    /// `thread_send` through the SDK: the same engine path a session tool takes.
    pub fn send(
        &self,
        thread: &str,
        text: &str,
        mode: Option<GatewayDeliveryMode>,
    ) -> BoxFuture<GatewayDeliveryResult> {
        let provenance = sdk_provenance(
            &self.options.cwd,
            &self.options.env,
            self.options.workspaces.clone(),
        );
        let provenance = Arc::new(provenance);
        let options = self.options.clone();
        let request = GatewayDeliverRequest {
            mode,
            ..GatewayDeliverRequest::new(self.sender(), thread, text)
        };
        Box::pin(async move {
            let engine = GatewayEngine::new(super::engine::GatewayEngineOptions {
                sender_provenance: Some(Arc::new(move || (*provenance).clone())),
                store: options.store.clone(),
                endpoints: options.engine.options().endpoints.clone(),
                resolve: options.engine.options().resolve.clone(),
                now: options.engine.options().now.clone(),
            });
            engine.deliver(request).await
        })
    }

    pub async fn list(&self, request: ThreadListInput) -> Value {
        let surface = self.surface_options();
        guarded(self.options.store.now(), async {
            let view = host_view(&surface, ThreadHostViewRequest::default()).await?;
            let listed =
                list_threads_async(&surface, &view, request.all_scope, request.state.as_deref())
                    .await;
            Ok(with_backlog(surface.store.as_ref(), listed))
        })
        .await
    }

    pub async fn read(&self, request: ThreadReadInput) -> Value {
        let surface = self.surface_options();
        guarded(self.options.store.now(), async {
            let view = host_view(&surface, ThreadHostViewRequest::default()).await?;
            answered(|| Box::pin(read_thread(&surface, &view, &request, UNKNOWN_CALLER))).await
        })
        .await
    }

    pub async fn create(&self, raw: Value) -> Value {
        let surface = Arc::new(self.surface_options());
        let sdk = self.clone();
        guarded(self.options.store.now(), async {
            let Some(schema) = thread_tool_param_schema("thread_create") else {
                return Err("missing schema".to_owned());
            };
            let value: ThreadCreateInput = match parse_thread_params(&schema, &raw) {
                Ok(v) => v,
                Err(ThreadDataError::Error { error }) => {
                    return Ok(json!({ "kind": "error", "error": error }));
                }
            };
            let op_id = value
                .idempotency_key
                .clone()
                .unwrap_or_else(|| format!("auto:{}", Uuid::new_v4()));
            let view = host_view(&surface, ThreadHostViewRequest::default()).await?;
            let surf_v = surface.clone();
            let surf_w = surface.clone();
            let wait_ms = wait_within_call_budget(
                FIRST_PROMPT_START_WAIT_MS as i64,
                Some(FIRST_PROMPT_START_RESERVE_MS as i64),
                None,
            )
            .max(0) as u64;
            create_thread(
                &surface,
                &view,
                &value,
                CreateContext {
                    operation_id: op_id,
                    caller_id: UNKNOWN_CALLER.to_owned(),
                    retrying: None,
                    view: Arc::new(move || {
                        let s = surf_v.clone();
                        Box::pin(
                            async move { host_view(&s, ThreadHostViewRequest::default()).await },
                        )
                    }),
                    deliver_prompt: Arc::new(move |_, target_id, send, _| {
                        let sdk = sdk.clone();
                        Box::pin(async move {
                            let mode = send
                                .delivery
                                .as_deref()
                                .and_then(GatewayDeliveryMode::parse);
                            let res = sdk.send(&target_id, &send.message, mode).await;
                            Ok(serde_json::to_value(res).unwrap_or(Value::Null))
                        })
                    }),
                    await_prompt_start: Arc::new(move |did| {
                        let st = surf_w.store.clone();
                        Box::pin(
                            async move { await_prompt_start(st.as_ref(), &did, wait_ms).await },
                        )
                    }),
                    record_creator: Arc::new(|_, _| Box::pin(async {})),
                },
            )
            .await
        })
        .await
    }

    pub async fn interrupt(&self, raw: Value) -> Value {
        let surface = self.surface_options();
        guarded(self.options.store.now(), async {
            let Some(schema) = thread_tool_param_schema("thread_interrupt") else {
                return Err("missing schema".to_owned());
            };
            let value: ThreadInterruptInput = match parse_thread_params(&schema, &raw) {
                Ok(v) => v,
                Err(ThreadDataError::Error { error }) => {
                    return Ok(json!({ "kind": "error", "error": error }));
                }
            };
            let view = host_view(&surface, ThreadHostViewRequest::default()).await?;
            answered(|| {
                Box::pin(async move {
                    match locate_control(
                        &surface,
                        &view,
                        &value.thread,
                        value.all_scope.unwrap_or(false),
                        UNKNOWN_CALLER,
                    )
                    .await
                    {
                        LocatedControl::Error(err) => Ok(err),
                        LocatedControl::NotLive { .. } => Ok(not_live_failure()),
                        LocatedControl::Control(ctrl) => {
                            interrupt_thread(ctrl.as_ref(), value.turn_id.as_deref()).await
                        }
                    }
                })
            })
            .await
        })
        .await
    }

    pub async fn close(&self, raw: Value) -> Value {
        let surface = self.surface_options();
        guarded(self.options.store.now(), async {
            let Some(schema) = thread_tool_param_schema("thread_close") else {
                return Err("missing schema".to_owned());
            };
            let value: ThreadCloseInput = match parse_thread_params(&schema, &raw) {
                Ok(v) => v,
                Err(ThreadDataError::Error { error }) => {
                    return Ok(json!({ "kind": "error", "error": error }));
                }
            };
            let view = host_view(&surface, ThreadHostViewRequest::default()).await?;
            answered(|| {
                Box::pin(async move {
                    match locate_control(
                        &surface,
                        &view,
                        value.thread.as_deref().unwrap_or("self"),
                        value.all_scope.unwrap_or(false),
                        UNKNOWN_CALLER,
                    )
                    .await
                    {
                        LocatedControl::Error(err) => Ok(err),
                        LocatedControl::NotLive {
                            thread_id, device, ..
                        } => {
                            let mut obj = control_identity(&thread_id, device.as_deref())
                                .as_object()
                                .cloned()
                                .unwrap_or_default();
                            obj.insert("kind".into(), Value::String("ok".into()));
                            obj.insert("closed".into(), Value::Bool(false));
                            obj.insert("reason".into(), Value::String("not_live".into()));
                            obj.insert("dropped_deliveries".into(), json!([]));
                            Ok(Value::Object(obj))
                        }
                        LocatedControl::Control(ctrl) => {
                            close_thread(ctrl.as_ref(), close_options(&value)).await
                        }
                    }
                })
            })
            .await
        })
        .await
    }

    pub async fn rename(&self, raw: Value) -> Value {
        let surface = self.surface_options();
        guarded(self.options.store.now(), async {
            let Some(schema) = thread_tool_param_schema("thread_rename") else {
                return Err("missing schema".to_owned());
            };
            let value: ThreadRenameInput = match parse_thread_params(&schema, &raw) {
                Ok(v) => v,
                Err(ThreadDataError::Error { error }) => {
                    return Ok(json!({ "kind": "error", "error": error }));
                }
            };
            let key = value
                .idempotency_key
                .clone()
                .unwrap_or_else(|| format!("auto:{}", Uuid::new_v4()));
            let view = host_view(&surface, ThreadHostViewRequest::default()).await?;
            answered(|| Box::pin(rename_thread(&surface, &view, &value, &key, UNKNOWN_CALLER)))
                .await
        })
        .await
    }

    pub async fn set_model(&self, raw: Value) -> Value {
        let surface = self.surface_options();
        guarded(self.options.store.now(), async {
            let Some(schema) = thread_tool_param_schema("thread_set_model") else {
                return Err("missing schema".to_owned());
            };
            let value: ThreadSetModelInput = match parse_thread_params(&schema, &raw) {
                Ok(v) => v,
                Err(ThreadDataError::Error { error }) => {
                    return Ok(json!({ "kind": "error", "error": error }));
                }
            };
            let view = host_view(&surface, ThreadHostViewRequest::default()).await?;
            answered(|| {
                Box::pin(async move {
                    match locate_control(
                        &surface,
                        &view,
                        &value.thread,
                        value.all_scope.unwrap_or(false),
                        UNKNOWN_CALLER,
                    )
                    .await
                    {
                        LocatedControl::Error(err) => Ok(err),
                        LocatedControl::NotLive { .. } => Ok(not_live_failure()),
                        LocatedControl::Control(ctrl) => {
                            set_model_on(ctrl.as_ref(), &value.model, value.provider.as_deref())
                                .await
                        }
                    }
                })
            })
            .await
        })
        .await
    }

    pub async fn locate(&self, thread: &str, all_scope: Option<bool>) -> Value {
        let surface = self.surface_options();
        guarded(self.options.store.now(), async {
            let view = host_view(&surface, ThreadHostViewRequest::default()).await?;
            let entries = address_book(&surface, &view, &[]);
            let ctx = ResolveContext {
                remote: None,
                caller_workspace_root: self.options.cwd.clone(),
                local: to_thread_address_entries(&entries),
                local_for: None,
                caller_id: UNKNOWN_CALLER.to_owned(),
            };
            match resolve_address(
                &ctx,
                thread,
                &ResolveOptions {
                    all_scope: all_scope.unwrap_or(false),
                    probe: true,
                    ..Default::default()
                },
            )
            .await
            {
                Err(err) => Ok(json!({ "kind": "error", "error": err.into_failure() })),
                Ok(resolved) => match resolved.where_located {
                    ResolveWhere::Device { row } => Ok(json!({
                        "kind": "error",
                        "error": thread_tool_failure(
                            "remote_unavailable",
                            format!("Thread {} is on device {}, which requires O02.", row.session_id, row.device),
                            "Locate a local thread.",
                            None,
                        )
                    })),
                    ResolveWhere::Here { .. } => {
                        let candidate = entries
                            .iter()
                            .filter(|e| e.thread_id == resolved.thread)
                            .find(|e| e.alive)
                            .or_else(|| entries.iter().find(|e| e.thread_id == resolved.thread));
                        let Some(entry) = candidate else {
                            return Ok(json!({
                                "kind": "error",
                                "error": thread_tool_failure(
                                    "not_found",
                                    format!("Thread {} is not in the address book.", resolved.thread),
                                    "Run omo thread list --all-scope.",
                                    None,
                                )
                            }));
                        };
                        Ok(json!({
                            "kind": "ok",
                            "thread": {
                                "thread_id": entry.thread_id,
                                "title": entry.title,
                                "cwd": entry.cwd,
                                "status": entry.status,
                                "session_path": entry.session_path,
                                "surface": entry.surface,
                                "alive": entry.alive,
                            }
                        }))
                    }
                },
            }
        })
        .await
    }

    pub fn report(&self, request: &ReportOpRequest) -> RelayResult<ReportOpResult> {
        self.options.relay.report(request)
    }

    pub fn bind(&self, input: &BindInput) -> RelayResult<BindingRecord> {
        self.options.relay.bind(input)
    }

    pub fn unbind(
        &self,
        binding_id: &str,
        expected_revision: i64,
    ) -> RelayResult<(BindingRecord, bool, Vec<String>)> {
        self.options.relay.unbind(binding_id, expected_revision)
    }

    pub fn rebind(
        &self,
        binding_id: &str,
        expected_revision: i64,
        session_durable_id: &str,
    ) -> RelayResult<(BindingRecord, Vec<String>)> {
        self.options
            .relay
            .rebind(binding_id, expected_revision, session_durable_id)
    }

    pub fn register_store_extension(
        &self,
        descriptor: &StoreExtensionRegistration,
    ) -> StoreExtensionResult<i64> {
        self.options
            .store
            .register_store_extension(descriptor, self.options.store.now(), None)
    }

    pub fn extension_call(
        &self,
        name: &str,
        op: &str,
        args: &Value,
    ) -> StoreExtensionResult<Value> {
        self.options
            .store
            .extension_call(name, op, args, self.options.store.now())
    }

    pub fn session_callable_ops(&self) -> Vec<DeclaredSessionOp> {
        self.options.store.session_callable_ops()
    }

    pub fn extension_session_await(
        &self,
        name: &str,
        op: &str,
        args: &Value,
        caller: &SessionCaller,
    ) -> StoreExtensionResult<Value> {
        self.options
            .store
            .extension_session_await(name, op, args, caller, self.options.store.now())
    }

    /// The session settled: armed completions become outbox rows.
    pub fn settle(
        &self,
        session_durable_id: &str,
        outcome: &str,
        through_arm_seq: Option<i64>,
    ) -> RelayResult<Vec<(String, i64)>> {
        self.options
            .relay
            .settle(session_durable_id, outcome, through_arm_seq)
    }
}

async fn guarded<F>(now: i64, body: F) -> Value
where
    F: std::future::Future<Output = Result<Value, String>>,
{
    match with_call_deadline(now + TOOL_CALL_BUDGET_MS, body).await {
        Ok(val) => val,
        Err(msg) => {
            if let Some(schema) = super::errors::gateway_schema_refusal_message(&msg) {
                return json!({ "kind": "error", "error": schema });
            }
            if let Some(rest) = msg.strip_prefix("host_unavailable:") {
                return json!({
                    "kind": "error",
                    "error": thread_tool_failure(
                        "host_unavailable",
                        format!("The thread host is unavailable at {rest}."),
                        "Retry when a host or terminal session is running.",
                        None,
                    )
                });
            }
            if let Some(cmd) = msg.strip_prefix("unsupported:") {
                return json!({
                    "kind": "error",
                    "error": thread_tool_failure(
                        "unsupported",
                        format!("The target's endpoint does not accept {cmd}."),
                        "Use thread read to follow the session.",
                        Some(json!({ "command": cmd })),
                    )
                });
            }
            json!({
                "kind": "error",
                "error": thread_tool_failure(
                    "internal_error",
                    format!("Thread operation failed: {msg}"),
                    "Run omo thread list and retry after checking the target.",
                    None,
                )
            })
        }
    }
}

/// A report the caller retries is refused with the same failure a relay reports.
pub fn report_failure(refusal: RelayOutcome<ReportOpResult>) -> Option<ThreadToolFailure> {
    match refusal {
        RelayOutcome::Ok { .. } => None,
        RelayOutcome::Refused {
            code,
            message,
            details,
        } => Some(super::errors::thread_tool_failure(
            code.as_str(),
            message,
            "Call thread_bindings and retry after checking the binding.",
            details,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_environment_labels_reach_the_sender_provenance() {
        let env = BTreeMap::from([
            (
                "OMO_PROVENANCE_WORKSPACE".to_owned(),
                "Runtime workspace".to_owned(),
            ),
            (
                "OMO_PROVENANCE_PROJECT".to_owned(),
                "runtime-project".to_owned(),
            ),
            ("OMO_PROVENANCE_LANE".to_owned(), "review".to_owned()),
        ]);
        let provenance = sdk_provenance("/tmp/whatever", &env, None);
        assert_eq!(provenance.workspace, "Runtime workspace");
        assert_eq!(provenance.project, "runtime-project");
        assert_eq!(provenance.lane.as_deref(), Some("review"));
        assert!(provenance.session.is_none());
    }
}
