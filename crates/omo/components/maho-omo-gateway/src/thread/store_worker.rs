//! The store worker protocol.
//!
//! Upstream counterpart: `thread/gateway/store-worker.ts`. The Rust store runs in-process, so this
//! module is the protocol both halves share: one request, one reply, no call ever replayed. A host
//! that moves the connection to another thread or process posts these frames instead of calling the
//! facade directly.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::store_api::GatewayStoreApi;
use super::store_extension_facade::ThreadCreationRecord;
use super::store_extensions::{SessionCaller, StoreExtensionRegistration};
use super::types::{ClaimRequest, EnqueueRequest, GatewayStoreConfig, RecordOutcomeRequest, ReconcileRequest, RefusalReason};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", content = "args", rename_all = "snake_case")]
pub enum WorkerRequest {
    Init { config: GatewayStoreConfig, now: i64 },
    Enqueue { request: Box<EnqueueRequest> },
    Reconcile { request: ReconcileRequest },
    Claim { request: ClaimRequest },
    RecordOutcome { request: RecordOutcomeRequest },
    RefuseQueued { now: i64, delivery_id: String, reason: RefusalReason },
    DeliveryView { delivery_id: String },
    List { target_durable_id: Option<String>, root_id: Option<String> },
    IsReferenced { durable_id: String },
    JournalMode,
    Stats,
    ExtensionRegister { extension: StoreExtensionRegistration, now: i64, restored_at: Option<i64> },
    ExtensionCall { name: String, op: String, args: Value, now: i64 },
    SessionCallableOps,
    ExtensionSessionCall { name: String, op: String, args: Value, caller: SessionCaller, now: i64 },
    RecordThreadCreation { record: ThreadCreationRecord, now: i64 },
    Close,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkerReply {
    Value { value: Value },
    Error { message: String, code: String },
    Closed,
}

impl WorkerReply {
    pub fn error(error: impl Into<String>, code: impl Into<String>) -> Self {
        Self::Error { message: error.into(), code: code.into() }
    }
}

fn value<T: Serialize>(value: &T) -> WorkerReply {
    WorkerReply::Value { value: serde_json::to_value(value).unwrap_or(Value::Null) }
}

/// Dispatches one frame onto the store; every failure is data, never a panic.
pub fn handle_request(store: &dyn GatewayStoreApi, request: WorkerRequest) -> WorkerReply {
    match request {
        WorkerRequest::Init { config, .. } => value(&serde_json::json!({ "agent_dir": config.agent_dir, "instance_id": config.instance_id })),
        WorkerRequest::Enqueue { request } => match store.enqueue(&request) {
            Ok(outcome) => value(&outcome),
            Err(error) => WorkerReply::error(error.message(), error.code()),
        },
        WorkerRequest::Reconcile { request } => match store.reconcile(&request) {
            Ok(outcome) => value(&outcome),
            Err(error) => WorkerReply::error(error.message(), error.code()),
        },
        WorkerRequest::Claim { request } => match store.claim(&request) {
            Ok(outcome) => value(&outcome),
            Err(error) => WorkerReply::error(error.message(), error.code()),
        },
        WorkerRequest::RecordOutcome { request } => match store.record_outcome(&request) {
            Ok(outcome) => value(&outcome),
            Err(error) => WorkerReply::error(error.message(), error.code()),
        },
        WorkerRequest::RefuseQueued { now, delivery_id, reason } => match store.refuse_queued(now, &delivery_id, reason) {
            Ok(changed) => value(&changed),
            Err(error) => WorkerReply::error(error.message(), error.code()),
        },
        WorkerRequest::DeliveryView { delivery_id } => match store.delivery_view(&delivery_id) {
            Ok(view) => value(&view),
            Err(error) => WorkerReply::error(error.message(), error.code()),
        },
        WorkerRequest::List { target_durable_id, root_id } => match store.list(target_durable_id.as_deref(), root_id.as_deref()) {
            Ok(rows) => value(&rows),
            Err(error) => WorkerReply::error(error.message(), error.code()),
        },
        WorkerRequest::IsReferenced { durable_id } => match store.is_referenced(&durable_id) {
            Ok(referenced) => value(&referenced),
            Err(error) => WorkerReply::error(error.message(), error.code()),
        },
        WorkerRequest::JournalMode => match store.journal_mode() {
            Ok(mode) => value(&mode),
            Err(error) => WorkerReply::error(error.message(), error.code()),
        },
        WorkerRequest::Stats => value(&store.stats()),
        WorkerRequest::ExtensionRegister { extension, now, restored_at } => value(&store.register_store_extension(&extension, now, restored_at)),
        WorkerRequest::ExtensionCall { name, op, args, now } => value(&store.extension_call(&name, &op, &args, now)),
        WorkerRequest::SessionCallableOps => value(&store.session_callable_ops()),
        WorkerRequest::ExtensionSessionCall { name, op, args, caller, now } => value(&store.extension_session_await(&name, &op, &args, &caller, now)),
        WorkerRequest::RecordThreadCreation { record, now } => match store.record_thread_creation(&record, now) {
            Ok(()) => value(&Value::Null),
            Err(error) => WorkerReply::error(error.message(), error.code()),
        },
        WorkerRequest::Close => WorkerReply::Closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_round_trips_through_json() {
        let request = WorkerRequest::Enqueue {
            request: Box::new(EnqueueRequest {
                provenance: super::super::types::Provenance { workspace: "Example".into(), project: "project".into(), lane: None, session: None },
                now: 1,
                delivery_id: "d-1".into(),
                target_durable_id: "receiver".into(),
                sender_principal: "session:sender".into(),
                rate_principal: None,
                sender_node: "sender".into(),
                sender_turn: None,
                cause_delivery_id: None,
                claimed_root_id: None,
                origin: super::super::types::EnvelopeOrigin::Session { session: "sender".into() },
                actor: "planner".into(),
                body: "hi".into(),
                mode: super::super::types::GatewayDeliveryMode::Auto,
                expected_turn_id: None,
                binding: None,
                receipt: super::super::types::ReceiptRef { idempotency_key: "k".into(), args_hash: "h".into() },
                endpoint_kind: None,
            }),
        };
        let json = serde_json::to_value(&request).expect("serialize");
        assert_eq!(json["op"], "enqueue");
        assert_eq!(serde_json::from_value::<WorkerRequest>(json).expect("deserialize"), request);
    }

    #[test]
    fn a_reply_carries_its_code() {
        assert_eq!(WorkerReply::error("boom", "gateway_lock_wait_exceeded"), WorkerReply::Error { message: "boom".into(), code: "gateway_lock_wait_exceeded".into() });
    }
}
