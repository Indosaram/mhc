//! The gateway store facade.
//!
//! Upstream counterpart: `thread/gateway/store.ts`. The Rust port owns one connection behind a
//! mutex instead of a worker thread: every call is synchronous and serialized, so no store call ever
//! runs on a session loop, and the worker protocol (`store-worker.ts`) is the seam a future thread
//! host would use to move the connection off-process.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;

use super::constants::{GATEWAY_BUSY_TIMEOUT_MS, GATEWAY_LOCK_WAIT_MAX_MS};
use super::extension_registrations::{list_session_ops, read_descriptor};
use super::extension_session_ops::{SessionCallPhase, SessionCallRequest, session_call};
use super::store_api::GatewayStoreApi;
use super::store_extension_facade::{ExtensionRegisterReply, ThreadCreationRecord, session_await};
use super::store_extension_ops::StoreExtensions;
use super::store_extensions::{
    DeclaredSessionOp, ExtensionModuleRegistry, SessionCaller, StoreExtensionRefusalCode,
    StoreExtensionRegistration, StoreExtensionResult,
};
use super::store_ops::{
    DeliveryReceipt, DeliveryView, SqlAccess, StoreContext, StoreError, migrate, open_connection,
    self_identity_now,
};
use super::store_relay_ops::{
    AnswerClaim, AnswerClaimRef, AnswerDelivered, BindInput, BindingRecord, BindingsFilter,
    OutboxPage, PriorAnswer, RelayOutcome, ReportOpRequest, ReportOpResult,
};
use super::types::{
    ClaimOutcome, ClaimRequest, DeliveryRow, EnqueueOutcome, EnqueueRequest, ExternalAuthor,
    GatewayStoreConfig, GatewayStoreStats, ProcessIdentity, ReconcileOutcome, ReconcileRequest,
    RecordOutcomeRequest, RefusalReason,
};
use crate::paths::gateway_database_path;

pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct GatewayStoreOptions {
    pub agent_dir: String,
    pub instance_id: Option<String>,
    pub runtime_instance: Option<String>,
    pub now: Option<Clock>,
    pub busy_timeout_ms: Option<u64>,
    pub lock_wait_max_ms: Option<u64>,
    pub modules: ExtensionModuleRegistry,
}

impl GatewayStoreOptions {
    pub fn new(agent_dir: impl Into<String>) -> Self {
        Self {
            agent_dir: agent_dir.into(),
            instance_id: None,
            runtime_instance: None,
            now: None,
            busy_timeout_ms: None,
            lock_wait_max_ms: None,
            modules: ExtensionModuleRegistry::new(),
        }
    }
}

pub struct SqliteGatewayStore {
    context: Mutex<StoreContext>,
    extensions: Mutex<StoreExtensions>,
    modules: ExtensionModuleRegistry,
    config: GatewayStoreConfig,
    clock: Clock,
    after_commit_listeners: Mutex<Vec<super::store_api::ExtensionErrorListener>>,
}

fn poisoned<T>(_: std::sync::PoisonError<T>) -> StoreError {
    StoreError::Message("the gateway store lock is poisoned".to_owned())
}

impl SqliteGatewayStore {
    fn context(&self) -> Result<MutexGuard<'_, StoreContext>, StoreError> {
        self.context.lock().map_err(poisoned)
    }

    fn extensions(&self) -> Result<MutexGuard<'_, StoreExtensions>, StoreError> {
        self.extensions.lock().map_err(poisoned)
    }

    /// The registrations the current worker holds, restored on the next worker after one exits.
    pub fn retained_registrations(
        &self,
    ) -> Result<Vec<super::store_extension_facade::RetainedRegistration>, StoreError> {
        let ctx = self.context()?;
        let extensions = self.extensions()?;
        let mut retained = Vec::new();
        for name in ctx
            .all(
                "SELECT name FROM extension_registrations ORDER BY name",
                &[],
            )?
            .iter()
            .map(|row| super::store_ops::row_str(row, "name"))
        {
            if let Some(descriptor) = read_descriptor(&*ctx, &name) {
                let registration = descriptor.named(&name);
                if extensions.holds(&registration) {
                    retained.push(super::store_extension_facade::RetainedRegistration {
                        extension: registration,
                        registered_at: self.now(),
                    });
                }
            }
        }
        Ok(retained)
    }

    /// A restarted worker replays a registration this process made at `registered_at`.
    pub fn restore_registration(
        &self,
        registration: &super::store_extension_facade::RetainedRegistration,
    ) -> ExtensionRegisterReply {
        let result = self.register_store_extension(
            &registration.extension,
            self.now(),
            Some(registration.registered_at),
        );
        let retained = result.is_ok();
        ExtensionRegisterReply { result, retained }
    }

    fn session_reply(
        &self,
        name: &str,
        op: &str,
        args: &Value,
        caller: &SessionCaller,
        now: i64,
        phase: SessionCallPhase,
    ) -> super::extension_session_ops::SessionCallReply {
        let ctx = match self.context() {
            Ok(ctx) => ctx,
            Err(error) => {
                return super::extension_session_ops::SessionCallReply {
                    result: StoreExtensionResult::refused(
                        StoreExtensionRefusalCode::ExtensionOperationFailed,
                        error.message(),
                    ),
                    await_wait: None,
                };
            }
        };
        let declared: Vec<DeclaredSessionOp> = list_session_ops(&*ctx);
        let Some(entry) = declared
            .into_iter()
            .find(|entry| entry.extension == name && entry.entry.op == op)
        else {
            return super::extension_session_ops::SessionCallReply {
                result: StoreExtensionResult::refused(
                    StoreExtensionRefusalCode::ExtensionUnknownOp,
                    format!("Extension {name} declares no session-callable op {op}."),
                ),
                await_wait: None,
            };
        };
        let descriptor = read_descriptor(&*ctx, name);
        let wake_dir = descriptor
            .as_ref()
            .and_then(|descriptor| descriptor.wake_dir.clone());
        let extensions = match self.extensions() {
            Ok(extensions) => extensions,
            Err(error) => {
                return super::extension_session_ops::SessionCallReply {
                    result: StoreExtensionResult::refused(
                        StoreExtensionRefusalCode::ExtensionOperationFailed,
                        error.message(),
                    ),
                    await_wait: None,
                };
            }
        };
        let registered = match extensions.load(&ctx, &self.modules, name) {
            Ok(registered) => registered,
            Err(refused) => {
                return super::extension_session_ops::SessionCallReply {
                    result: refused,
                    await_wait: None,
                };
            }
        };
        let request = SessionCallRequest {
            name: name.to_owned(),
            op: op.to_owned(),
            phase,
            args: args.clone(),
            caller: caller.caller_durable_id.clone(),
            now,
        };
        let reply = session_call(&ctx, &entry, wake_dir.as_deref(), &request, &|op, args| {
            extensions.run(
                &ctx,
                &registered.descriptor,
                &registered.module,
                op,
                args,
                now,
            )
        });
        drop(extensions);
        drop(ctx);
        if phase == SessionCallPhase::Op && reply.result.is_ok()
            && let (Some(wake_dir), Some(wake)) = (wake_dir.as_deref(), entry.entry.wake.as_deref())
                && let Err(error) = super::extension_session_ops::touch_wake(
                    Path::new(&self.config.agent_dir),
                    wake_dir,
                    wake,
                ) {
                    let listeners = self
                        .after_commit_listeners
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .clone();
                    for listener in listeners {
                        listener(name, &error.message());
                    }
                }
        reply
    }
}

/// Opens (and migrates) the store under an agent directory.
pub fn create_gateway_store(
    options: GatewayStoreOptions,
) -> Result<Arc<SqliteGatewayStore>, StoreError> {
    let clock = options.now.unwrap_or_else(|| Arc::new(default_clock));
    let config = GatewayStoreConfig {
        agent_dir: options.agent_dir.clone(),
        busy_timeout_ms: options.busy_timeout_ms.unwrap_or(GATEWAY_BUSY_TIMEOUT_MS),
        lock_wait_max_ms: options.lock_wait_max_ms.unwrap_or(GATEWAY_LOCK_WAIT_MAX_MS),
        instance_id: options
            .instance_id
            .unwrap_or_else(|| super::store_ops::new_id("instance")),
        runtime_instance: options.runtime_instance,
        legacy_mailbox_directories: Vec::new(),
        test_hooks: Default::default(),
    };
    let path = gateway_database_path(Path::new(&options.agent_dir));
    let conn = open_connection(&path, config.busy_timeout_ms)?;
    let identity = ProcessIdentity {
        pid: std::process::id() as i64,
        process_start_time: self_identity_now(),
        instance_id: config.instance_id.clone(),
        runtime_instance: config.runtime_instance.clone(),
    };
    let context = StoreContext::new(conn, config.clone(), identity);
    migrate(&context)?;
    Ok(Arc::new(SqliteGatewayStore {
        context: Mutex::new(context),
        extensions: Mutex::new(StoreExtensions::new()),
        modules: options.modules,
        config,
        clock,
        after_commit_listeners: Mutex::new(Vec::new()),
    }))
}

fn default_clock() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

impl GatewayStoreApi for SqliteGatewayStore {
    fn on_after_commit_error(&self, listener: super::store_api::ExtensionErrorListener) {
        self.after_commit_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(listener);
    }
    fn busy_timeout_ms(&self) -> u64 {
        self.config.busy_timeout_ms
    }

    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn identity(&self) -> Result<ProcessIdentity, StoreError> {
        Ok(self.context()?.self_identity.clone())
    }

    fn enqueue(&self, request: &EnqueueRequest) -> Result<EnqueueOutcome, StoreError> {
        super::store_ops::enqueue(&*self.context()?, request)
    }

    fn reconcile(&self, request: &ReconcileRequest) -> Result<ReconcileOutcome, StoreError> {
        super::store_ops::reconcile(&*self.context()?, request)
    }

    fn claim(&self, request: &ClaimRequest) -> Result<ClaimOutcome, StoreError> {
        super::store_ops::claim(&*self.context()?, request)
    }

    fn record_outcome(&self, request: &RecordOutcomeRequest) -> Result<ClaimOutcome, StoreError> {
        super::store_ops::record_outcome(&*self.context()?, request)
    }

    fn refuse_queued(
        &self,
        now: i64,
        delivery_id: &str,
        reason: RefusalReason,
    ) -> Result<bool, StoreError> {
        super::store_ops::refuse_queued(&*self.context()?, now, delivery_id, reason)
    }

    fn complete_receipt(
        &self,
        now: i64,
        principal: &str,
        idempotency_key: &str,
        result: &Value,
    ) -> Result<bool, StoreError> {
        super::store_ops::complete_receipt(
            &*self.context()?,
            now,
            principal,
            idempotency_key,
            result,
        )
    }

    fn abandon_receipt(
        &self,
        now: i64,
        principal: &str,
        idempotency_key: &str,
        error_note: &str,
    ) -> Result<bool, StoreError> {
        super::store_ops::abandon_receipt(
            &*self.context()?,
            now,
            principal,
            idempotency_key,
            error_note,
        )
    }

    fn delivery_view(&self, delivery_id: &str) -> Result<Option<DeliveryView>, StoreError> {
        super::store_ops::delivery_view(&*self.context()?, delivery_id)
    }

    fn delivery_receipt(
        &self,
        now: i64,
        principal: &str,
        idempotency_key: &str,
    ) -> Result<Option<DeliveryReceipt>, StoreError> {
        super::store_ops::delivery_receipt(&*self.context()?, now, principal, idempotency_key)
    }

    fn recover_delivery(
        &self,
        now: i64,
        principal: &str,
        idempotency_key: &str,
        args_hash: &str,
    ) -> Result<Option<EnqueueOutcome>, StoreError> {
        super::store_ops::recover_delivery(
            &*self.context()?,
            now,
            principal,
            idempotency_key,
            args_hash,
        )
    }

    fn list(
        &self,
        target_durable_id: Option<&str>,
        root_id: Option<&str>,
    ) -> Result<Vec<DeliveryRow>, StoreError> {
        super::store_ops::list_deliveries(&*self.context()?, target_durable_id, root_id)
    }

    fn is_referenced(&self, durable_id: &str) -> Result<bool, StoreError> {
        super::store_ops::is_referenced(&*self.context()?, durable_id)
    }

    fn journal_mode(&self) -> Result<String, StoreError> {
        super::store_ops::journal_mode(&*self.context()?)
    }

    fn stats(&self) -> GatewayStoreStats {
        self.context
            .lock()
            .map(|ctx| ctx.stats())
            .unwrap_or_default()
    }

    fn bind(&self, now: i64, input: &BindInput) -> Result<RelayOutcome<BindingRecord>, StoreError> {
        super::store_relay_ops::bind_thread(&*self.context()?, now, input)
    }

    fn unbind(
        &self,
        now: i64,
        binding_id: &str,
        expected_revision: i64,
    ) -> Result<RelayOutcome<(BindingRecord, bool, Vec<String>)>, StoreError> {
        super::store_relay_ops::unbind_thread(&*self.context()?, now, binding_id, expected_revision)
    }

    fn rebind(
        &self,
        now: i64,
        binding_id: &str,
        expected_revision: i64,
        session_durable_id: &str,
    ) -> Result<RelayOutcome<(BindingRecord, Vec<String>)>, StoreError> {
        super::store_relay_ops::rebind_thread(
            &*self.context()?,
            now,
            binding_id,
            expected_revision,
            session_durable_id,
        )
    }

    fn list_bindings(
        &self,
        now: i64,
        filter: &BindingsFilter,
        cursor: Option<&str>,
        limit: Option<i64>,
    ) -> Result<RelayOutcome<(Vec<BindingRecord>, Option<String>)>, StoreError> {
        super::store_relay_ops::list_bindings(&*self.context()?, now, filter, cursor, limit)
    }

    fn binding_view(
        &self,
        now: i64,
        binding_id: &str,
    ) -> Result<Option<BindingRecord>, StoreError> {
        super::store_relay_ops::binding_view(&*self.context()?, now, binding_id)
    }

    fn report(
        &self,
        now: i64,
        request: &ReportOpRequest,
    ) -> Result<RelayOutcome<ReportOpResult>, StoreError> {
        super::store_relay_ops::report_event(&*self.context()?, now, request)
    }

    fn emit_completions(
        &self,
        now: i64,
        session_durable_id: &str,
        outcome: &str,
        through_arm_seq: Option<i64>,
    ) -> Result<Vec<(String, i64)>, StoreError> {
        super::store_relay_ops::emit_completions(
            &*self.context()?,
            now,
            session_durable_id,
            outcome,
            through_arm_seq,
        )
    }

    fn pending_completion_arms(&self, durable_id: &str) -> Result<i64, StoreError> {
        super::store_relay_ops::pending_completion_arms(&*self.context()?, durable_id)
    }

    fn latest_completion_arm(&self, durable_id: &str) -> Result<Option<i64>, StoreError> {
        super::store_relay_ops::latest_completion_arm(&*self.context()?, durable_id)
    }

    fn read_outbox(
        &self,
        now: i64,
        binding_id: &str,
        after_cursor: Option<i64>,
        limit: Option<i64>,
    ) -> Result<RelayOutcome<OutboxPage>, StoreError> {
        super::store_relay_ops::read_outbox(
            &*self.context()?,
            now,
            binding_id,
            after_cursor,
            limit,
            false,
        )
    }

    fn ack_outbox(
        &self,
        now: i64,
        binding_id: &str,
        cursor: i64,
        provider_message_id: Option<&str>,
    ) -> Result<RelayOutcome<(i64, bool)>, StoreError> {
        super::store_relay_ops::ack_outbox(
            &*self.context()?,
            now,
            binding_id,
            cursor,
            provider_message_id,
        )
    }

    fn claim_answer(
        &self,
        now: i64,
        binding_id: &str,
        reply_token: &str,
        answer: &str,
        answered_by: Option<&ExternalAuthor>,
    ) -> Result<RelayOutcome<AnswerClaim>, StoreError> {
        super::store_relay_ops::claim_answer(
            &*self.context()?,
            now,
            binding_id,
            reply_token,
            answer,
            answered_by,
        )
    }

    fn release_answer(&self, claim: &AnswerClaimRef) -> Result<bool, StoreError> {
        super::store_relay_ops::release_answer(&*self.context()?, claim)
    }

    fn confirm_answer(&self, delivered: &AnswerDelivered) -> Result<bool, StoreError> {
        super::store_relay_ops::confirm_answer(&*self.context()?, delivered)
    }

    fn mark_prior_delivered(
        &self,
        claim: &AnswerClaimRef,
        prior: &PriorAnswer,
    ) -> Result<bool, StoreError> {
        super::store_relay_ops::mark_prior_delivered(&*self.context()?, claim, prior)
    }

    fn close_question(
        &self,
        now: i64,
        session_durable_id: &str,
        ui_request_id: &str,
    ) -> Result<i64, StoreError> {
        super::store_relay_ops::close_question(
            &*self.context()?,
            now,
            session_durable_id,
            ui_request_id,
        )
    }

    fn register_incarnation(
        &self,
        durable_id: &str,
        incarnation: &str,
        endpoint_socket: Option<&str>,
        endpoint_kind: Option<&str>,
    ) -> Result<(), StoreError> {
        super::store_relay_ops::register_incarnation(
            &*self.context()?,
            durable_id,
            incarnation,
            endpoint_socket,
            endpoint_kind,
        )
    }

    fn clear_endpoint(&self, durable_id: &str, incarnation: &str) -> Result<(), StoreError> {
        super::store_relay_ops::clear_endpoint(&*self.context()?, durable_id, incarnation)
    }

    fn session_endpoint(
        &self,
        durable_id: &str,
    ) -> Result<Option<(String, String, String)>, StoreError> {
        super::store_relay_ops::session_endpoint(&*self.context()?, durable_id)
    }

    fn register_store_extension(
        &self,
        descriptor: &StoreExtensionRegistration,
        now: i64,
        restored_at: Option<i64>,
    ) -> StoreExtensionResult<i64> {
        let ctx = match self.context() {
            Ok(ctx) => ctx,
            Err(error) => {
                return StoreExtensionResult::refused(
                    StoreExtensionRefusalCode::ExtensionOperationFailed,
                    error.message(),
                );
            }
        };
        let mut extensions = match self.extensions() {
            Ok(extensions) => extensions,
            Err(error) => {
                return StoreExtensionResult::refused(
                    StoreExtensionRefusalCode::ExtensionOperationFailed,
                    error.message(),
                );
            }
        };
        extensions.register(&ctx, &self.modules, descriptor, now, restored_at)
    }

    fn extension_call(
        &self,
        name: &str,
        op: &str,
        args: &Value,
        now: i64,
    ) -> StoreExtensionResult<Value> {
        let ctx = match self.context() {
            Ok(ctx) => ctx,
            Err(error) => {
                return StoreExtensionResult::refused(
                    StoreExtensionRefusalCode::ExtensionOperationFailed,
                    error.message(),
                );
            }
        };
        let extensions = match self.extensions() {
            Ok(extensions) => extensions,
            Err(error) => {
                return StoreExtensionResult::refused(
                    StoreExtensionRefusalCode::ExtensionOperationFailed,
                    error.message(),
                );
            }
        };
        extensions.call(&ctx, &self.modules, name, op, args, now)
    }

    fn session_callable_ops(&self) -> Vec<DeclaredSessionOp> {
        match self.context() {
            Ok(ctx) => list_session_ops(&*ctx),
            Err(_) => Vec::new(),
        }
    }

    fn extension_session_await(
        &self,
        name: &str,
        op: &str,
        args: &Value,
        caller: &SessionCaller,
        now: i64,
    ) -> StoreExtensionResult<Value> {
        let reply = self.session_reply(name, op, args, caller, now, SessionCallPhase::Op);
        let Some(wait) = reply.await_wait else {
            return reply.result;
        };
        let value = match &reply.result {
            StoreExtensionResult::Ok { value } => value,
            refused => return refused.clone(),
        };
        let Some(id) = value.get("await_request_id").and_then(Value::as_str) else {
            return reply.result;
        };
        let status = || {
            self.session_reply(
                name,
                op,
                &serde_json::json!({ "await_request_id": id }),
                caller,
                self.now(),
                SessionCallPhase::Status,
            )
            .result
        };
        let expire = || {
            self.session_reply(
                name,
                op,
                &serde_json::json!({ "await_request_id": id }),
                caller,
                self.now(),
                SessionCallPhase::Expire,
            )
            .result
        };
        session_await(
            Path::new(&self.config.agent_dir),
            &wait.wake_dir,
            id,
            wait.timeout_ms,
            &status,
            &expire,
        )
    }

    fn record_thread_creation(
        &self,
        record: &ThreadCreationRecord,
        now: i64,
    ) -> Result<(), StoreError> {
        super::store_extension_session_ops::record_thread_creation(
            &*self.context()?,
            now,
            &record.creator_durable_id,
            &record.created_durable_id,
        )
    }
}

/// The modules a store resolves its extension registrations against.
pub fn module_registry(
    modules: BTreeMap<String, BTreeMap<String, super::store_extensions::StoreExtensionOperation>>,
) -> ExtensionModuleRegistry {
    modules
}
