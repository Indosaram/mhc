//! Control-endpoint registration and lifecycle (`thread/gateway/registration.ts`).

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use uuid::Uuid;

use super::adapter::{
    AdmitExternalMessageInput, AdmitResult, BoxFuture, DrainWakeEvent, SessionAdmissionGate,
    SessionRuntimePort, WakeReason, to_session_control_drain_result,
};
use super::drain::{AdmitIntakeFn, InboxDrain, InboxDrainOptions};
use super::endpoint_registry::endpoint_kind_of;
use super::store::{GatewayStoreOptions, create_gateway_store};
use super::store_api::GatewayStoreApi;
use super::types::AdmissionLedger;

pub type ControlEndpointRegistrar =
    Arc<dyn Fn(RegisterControlOptions) -> BoxFuture<ControlRegistrationReply> + Send + Sync>;

#[derive(Clone)]
pub struct RegisterControlOptions {
    pub inbox_dir: String,
    pub drain: Arc<dyn Fn(DrainWakeEvent) -> BoxFuture<Value> + Send + Sync>,
    pub is_session_referenced: Arc<dyn Fn() -> bool + Send + Sync>,
}

pub enum ControlRegistrationReply {
    Registered {
        socket: String,
        dispose: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>,
    },
    Unsupported {
        reason: String,
    },
    Failed {
        reason: String,
    },
}

#[derive(Clone)]
pub struct SessionControlActionsPort {
    pub register_control_endpoint: ControlEndpointRegistrar,
    pub admission_gate: Arc<dyn Fn() -> SessionAdmissionGate + Send + Sync>,
    pub admit_external_message:
        Arc<dyn Fn(AdmitExternalMessageInput) -> Result<AdmitResult, String> + Send + Sync>,
    pub list_admitted_deliveries: Arc<dyn Fn() -> AdmissionLedger + Send + Sync>,
    pub persist_header_now: Arc<dyn Fn() -> BoxFuture<Result<(), String>> + Send + Sync>,
}

pub type RegistrationNotifyFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone)]
pub struct ControlSession {
    pub durable_id: String,
    pub session_path: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub is_idle: Arc<dyn Fn() -> bool + Send + Sync>,
    pub notify: Option<RegistrationNotifyFn>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationOutcome {
    Registered { socket: String },
    AlreadyRegistered,
    Unsupported { reason: String },
    Failed { reason: String },
}

/// Host generation from `ExtensionSessionProfile::session_context["host_instance"]`.
pub fn host_instance_of(
    session_context: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    session_context
        .get("host_instance")
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Host socket from `ExtensionSessionProfile::session_context["host_socket"]`.
pub fn host_socket_of(
    session_context: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    session_context
        .get("host_socket")
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

pub fn parse_wake_reason(raw: &str) -> WakeReason {
    match raw {
        "idle" => WakeReason::Idle,
        "submission" => WakeReason::Submission,
        "draft_cleared" => WakeReason::DraftCleared,
        "command" => WakeReason::Command,
        "inbox" => WakeReason::Inbox,
        "emitted" => WakeReason::Emitted,
        "continue" => WakeReason::Continue,
        "start" => WakeReason::Start,
        _ => WakeReason::Inbox,
    }
}

pub fn drain_event_from_wake(reason: &str, delivery_ids: Option<&[String]>) -> DrainWakeEvent {
    DrainWakeEvent {
        reason: parse_wake_reason(reason),
        delivery_ids: delivery_ids.map(|ids| ids.to_vec()),
    }
}

pub struct ControlEndpointRegistrantOptions {
    pub control: SessionControlActionsPort,
    pub agent_dir: Arc<dyn Fn() -> String + Send + Sync>,
    pub runtime_instance: Option<String>,
    pub store: Option<Arc<dyn GatewayStoreApi>>,
    pub on_command_wake: Option<Arc<dyn Fn(String) -> BoxFuture<()> + Send + Sync>>,
    pub admit_intake: AdmitIntakeFn,
    pub log: Option<RegistrationNotifyFn>,
}

struct ActiveRegistration {
    durable_id: String,
    incarnation: String,
    drain: Arc<InboxDrain>,
    dispose: Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>,
    retired: Arc<std::sync::atomic::AtomicBool>,
}

pub struct ControlEndpointRegistrant {
    options: Arc<ControlEndpointRegistrantOptions>,
    store: Mutex<Option<Arc<dyn GatewayStoreApi>>>,
    shared_store: bool,
    active: Mutex<Option<ActiveRegistration>>,
    compacting: Arc<std::sync::atomic::AtomicBool>,
    open_questions: Arc<Mutex<HashSet<String>>>,
    serialize: tokio::sync::Mutex<()>,
}

impl ControlEndpointRegistrant {
    pub fn new(options: ControlEndpointRegistrantOptions) -> Arc<Self> {
        let shared = options.store.is_some();
        let initial_store = options.store.clone();
        Arc::new(Self {
            options: Arc::new(options),
            store: Mutex::new(initial_store),
            shared_store: shared,
            active: Mutex::new(None),
            compacting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            open_questions: Arc::new(Mutex::new(HashSet::new())),
            serialize: tokio::sync::Mutex::new(()),
        })
    }

    fn log_line(&self, line: &str) {
        if let Some(log) = &self.options.log {
            log(line);
        }
    }

    async fn release_active(&self) {
        let current = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(current) = current else {
            return;
        };
        (current.dispose)().await;
        current
            .retired
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let store = self
            .store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(store) = store {
            let _ = store.clear_endpoint(&current.durable_id, &current.incarnation);
        }
    }

    pub async fn start(&self, session: ControlSession) -> RegistrationOutcome {
        let _guard = self.serialize.lock().await;
        if let Some(active) = self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            && active.durable_id == session.durable_id
        {
            return RegistrationOutcome::AlreadyRegistered;
        }
        self.release_active().await;
        if let Err(reason) = (self.options.control.persist_header_now)().await {
            self.log_line(&format!(
                "thread gateway: the session header could not be persisted, so the session is not registered: {reason}"
            ));
            return RegistrationOutcome::Failed { reason };
        }
        let agent_dir = (self.options.agent_dir)();
        let store = {
            let mut guard = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(existing) = guard.as_ref() {
                existing.clone()
            } else {
                let mut opts = GatewayStoreOptions::new(&agent_dir);
                opts.runtime_instance = self.options.runtime_instance.clone();
                match create_gateway_store(opts) {
                    Ok(created) => {
                        *guard = Some(created.clone());
                        created
                    }
                    Err(error) => {
                        let reason = error.to_string();
                        self.log_line(&format!(
                            "thread gateway: gateway store failed to open: {reason}"
                        ));
                        return RegistrationOutcome::Failed { reason };
                    }
                }
            }
        };

        let compacting = self.compacting.clone();
        let is_idle = session.is_idle.clone();
        let open_questions = self.open_questions.clone();
        let admission_gate = self.options.control.admission_gate.clone();
        let admit_external = self.options.control.admit_external_message.clone();
        let list_admitted = self.options.control.list_admitted_deliveries.clone();

        let runtime = SessionRuntimePort {
            phase: Arc::new(move || {
                if compacting.load(std::sync::atomic::Ordering::Acquire) {
                    super::adapter::RuntimePhase::Compacting
                } else if is_idle() {
                    super::adapter::RuntimePhase::Idle
                } else if !open_questions
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .is_empty()
                {
                    super::adapter::RuntimePhase::WaitingQuestion
                } else {
                    super::adapter::RuntimePhase::MidTurn
                }
            }),
            admission_gate,
            admit_external_message: admit_external,
            list_admitted_deliveries: list_admitted,
        };

        let wake_store = store.clone();
        let durable_for_gate = session.durable_id.clone();
        let drain = Arc::new(InboxDrain::new(InboxDrainOptions {
            store: store.clone(),
            runtime,
            durable_id: session.durable_id.clone(),
            session_path: session.session_path.clone(),
            now: None,
            log: self.options.log.clone(),
            notify: session.notify.clone(),
            on_admitted: None,
            admit_intake: self.options.admit_intake.clone(),
            wake_gated: Some(Arc::new(move || {
                wake_store.woken_gated(&durable_for_gate).unwrap_or(false)
            })),
        }));

        let retired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let holder_ready = Arc::new(tokio::sync::Notify::new());
        let holder_ok = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let holder_set = Arc::new(std::sync::atomic::AtomicBool::new(false));

        let retired_d = retired.clone();
        let holder_ready_d = holder_ready.clone();
        let holder_ok_d = holder_ok.clone();
        let holder_set_d = holder_set.clone();
        let drain_d = drain.clone();
        let on_cmd_wake = self.options.on_command_wake.clone();
        let durable_w = session.durable_id.clone();

        let inbox_dir = crate::paths::gateway_inbox_directory(
            std::path::Path::new(&agent_dir),
            &session.durable_id,
        )
        .to_string_lossy()
        .into_owned();
        let drain_ref = drain.clone();

        let reply = (self.options.control.register_control_endpoint)(RegisterControlOptions {
            inbox_dir,
            drain: Arc::new(move |event| {
                let retired_d = retired_d.clone();
                let holder_ready_d = holder_ready_d.clone();
                let holder_ok_d = holder_ok_d.clone();
                let holder_set_d = holder_set_d.clone();
                let drain_d = drain_d.clone();
                let on_cmd_wake = on_cmd_wake.clone();
                let durable_w = durable_w.clone();
                Box::pin(async move {
                    if retired_d.load(std::sync::atomic::Ordering::Acquire) {
                        return json!({ "admitted": [] });
                    }
                    while !holder_set_d.load(std::sync::atomic::Ordering::Acquire) {
                        holder_ready_d.notified().await;
                    }
                    if !holder_ok_d.load(std::sync::atomic::Ordering::Acquire)
                        || retired_d.load(std::sync::atomic::Ordering::Acquire)
                    {
                        return json!({ "admitted": [] });
                    }
                    if event.reason == WakeReason::Command
                        && let Some(cb) = on_cmd_wake
                    {
                        cb(durable_w).await;
                    }
                    match drain_d.drain_gateway(&event) {
                        Ok(res) => {
                            let val = to_session_control_drain_result(&res);
                            serde_json::to_value(val).unwrap_or_else(|_| json!({ "admitted": [] }))
                        }
                        Err(_) => json!({ "admitted": [] }),
                    }
                })
            }),
            is_session_referenced: Arc::new(move || drain_ref.is_session_referenced()),
        })
        .await;

        let (socket, dispose) = match reply {
            ControlRegistrationReply::Registered { socket, dispose } => (socket, dispose),
            ControlRegistrationReply::Unsupported { reason } => {
                retired.store(true, std::sync::atomic::Ordering::Release);
                holder_ok.store(false, std::sync::atomic::Ordering::Release);
                holder_set.store(true, std::sync::atomic::Ordering::Release);
                holder_ready.notify_waiters();
                return RegistrationOutcome::Unsupported { reason };
            }
            ControlRegistrationReply::Failed { reason } => {
                retired.store(true, std::sync::atomic::Ordering::Release);
                holder_ok.store(false, std::sync::atomic::Ordering::Release);
                holder_set.store(true, std::sync::atomic::Ordering::Release);
                holder_ready.notify_waiters();
                self.log_line(&format!(
                    "thread gateway: the control endpoint for {} failed to register: {reason}",
                    session.durable_id
                ));
                return RegistrationOutcome::Failed { reason };
            }
        };

        let incarnation = Uuid::new_v4().to_string();
        let kind = endpoint_kind_of(&socket, &[]);
        if let Err(err) = store.register_incarnation(
            &session.durable_id,
            &incarnation,
            Some(&socket),
            Some(kind.as_str()),
        ) {
            retired.store(true, std::sync::atomic::Ordering::Release);
            holder_ok.store(false, std::sync::atomic::Ordering::Release);
            holder_set.store(true, std::sync::atomic::Ordering::Release);
            holder_ready.notify_waiters();
            let reason = err.to_string();
            self.log_line(&format!(
                "thread gateway: the session incarnation for {} could not be recorded, so the session is not registered: {reason}",
                session.durable_id
            ));
            dispose().await;
            return RegistrationOutcome::Failed { reason };
        }

        holder_ok.store(true, std::sync::atomic::Ordering::Release);
        holder_set.store(true, std::sync::atomic::Ordering::Release);
        holder_ready.notify_waiters();

        *self
            .active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ActiveRegistration {
            durable_id: session.durable_id,
            incarnation,
            drain,
            dispose,
            retired,
        });

        RegistrationOutcome::Registered { socket }
    }

    pub fn note_compaction(&self, active: bool) {
        self.compacting
            .store(active, std::sync::atomic::Ordering::Release);
    }

    pub fn note_question(&self, id: &str, waiting: bool) {
        let mut guard = self
            .open_questions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if waiting {
            guard.insert(id.to_owned());
        } else {
            guard.remove(id);
        }
    }

    pub async fn stop(&self) {
        let _guard = self.serialize.lock().await;
        self.release_active().await;
        if !self.shared_store {
            let owned = self
                .store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            drop(owned);
        }
    }

    pub fn current_drain(&self) -> Option<Arc<InboxDrain>> {
        self.active
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|a| a.drain.clone())
    }
}
