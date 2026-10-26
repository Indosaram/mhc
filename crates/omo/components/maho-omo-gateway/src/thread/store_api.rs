//! The gateway store facade's API.
//!
//! Upstream counterpart: `thread/gateway/store-api.ts`. Every call is synchronous: the store owns one
//! connection and the worker protocol serializes calls onto it.

pub type BindingPageResult = Result<RelayOutcome<(Vec<BindingRecord>, Option<String>)>, StoreError>;
use std::sync::Arc;

use serde_json::Value;

use super::store_extensions::{
    DeclaredSessionOp, SessionCaller, StoreExtensionRegistration, StoreExtensionResult,
};
use super::store_ops::{DeliveryReceipt, DeliveryView, StoreError};
use super::store_relay_ops::{
    AnswerClaim, AnswerClaimRef, AnswerDelivered, BindInput, BindingRecord, BindingsFilter,
    OutboxPage, PriorAnswer, RelayOutcome, ReportOpRequest, ReportOpResult,
};
use super::types::{
    ClaimOutcome, ClaimRequest, DeliveryRow, EnqueueOutcome, EnqueueRequest, ExternalAuthor,
    GatewayStoreStats, ProcessIdentity, ReconcileOutcome, ReconcileRequest, RecordOutcomeRequest,
    RefusalReason,
};

pub type ExtensionErrorListener = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub trait GatewayStoreApi: Send + Sync {
    fn on_after_commit_error(&self, _listener: ExtensionErrorListener) {}
    /// The store's busy timeout: the delay before a caller re-arms an operation that failed with a lock-wait error.
    fn busy_timeout_ms(&self) -> u64;
    /// The clock this store's rows are stamped and expired against.
    fn now(&self) -> i64;
    fn identity(&self) -> Result<ProcessIdentity, StoreError>;
    fn enqueue(&self, request: &EnqueueRequest) -> Result<EnqueueOutcome, StoreError>;
    fn reconcile(&self, request: &ReconcileRequest) -> Result<ReconcileOutcome, StoreError>;
    fn claim(&self, request: &ClaimRequest) -> Result<ClaimOutcome, StoreError>;
    fn record_outcome(&self, request: &RecordOutcomeRequest) -> Result<ClaimOutcome, StoreError>;
    fn refuse_queued(
        &self,
        now: i64,
        delivery_id: &str,
        reason: RefusalReason,
    ) -> Result<bool, StoreError>;
    fn complete_receipt(
        &self,
        now: i64,
        principal: &str,
        idempotency_key: &str,
        result: &Value,
    ) -> Result<bool, StoreError>;
    fn abandon_receipt(
        &self,
        now: i64,
        principal: &str,
        idempotency_key: &str,
        error_note: &str,
    ) -> Result<bool, StoreError>;
    fn delivery_view(&self, delivery_id: &str) -> Result<Option<DeliveryView>, StoreError>;
    fn delivery_receipt(
        &self,
        now: i64,
        principal: &str,
        idempotency_key: &str,
    ) -> Result<Option<DeliveryReceipt>, StoreError>;
    fn recover_delivery(
        &self,
        now: i64,
        principal: &str,
        idempotency_key: &str,
        args_hash: &str,
    ) -> Result<Option<EnqueueOutcome>, StoreError>;
    fn list(
        &self,
        target_durable_id: Option<&str>,
        root_id: Option<&str>,
    ) -> Result<Vec<DeliveryRow>, StoreError>;
    fn is_referenced(&self, durable_id: &str) -> Result<bool, StoreError>;
    fn journal_mode(&self) -> Result<String, StoreError>;
    fn stats(&self) -> GatewayStoreStats;

    fn bind(&self, now: i64, input: &BindInput) -> Result<RelayOutcome<BindingRecord>, StoreError>;
    fn unbind(
        &self,
        now: i64,
        binding_id: &str,
        expected_revision: i64,
    ) -> Result<RelayOutcome<(BindingRecord, bool, Vec<String>)>, StoreError>;
    fn rebind(
        &self,
        now: i64,
        binding_id: &str,
        expected_revision: i64,
        session_durable_id: &str,
    ) -> Result<RelayOutcome<(BindingRecord, Vec<String>)>, StoreError>;
    fn list_bindings(
        &self,
        now: i64,
        filter: &BindingsFilter,
        cursor: Option<&str>,
        limit: Option<i64>,
    ) -> BindingPageResult;
    fn binding_view(&self, now: i64, binding_id: &str)
    -> Result<Option<BindingRecord>, StoreError>;
    fn report(
        &self,
        now: i64,
        request: &ReportOpRequest,
    ) -> Result<RelayOutcome<ReportOpResult>, StoreError>;
    fn emit_completions(
        &self,
        now: i64,
        session_durable_id: &str,
        outcome: &str,
        through_arm_seq: Option<i64>,
    ) -> Result<Vec<(String, i64)>, StoreError>;
    fn pending_completion_arms(&self, durable_id: &str) -> Result<i64, StoreError>;
    fn latest_completion_arm(&self, durable_id: &str) -> Result<Option<i64>, StoreError>;
    fn read_outbox(
        &self,
        now: i64,
        binding_id: &str,
        after_cursor: Option<i64>,
        limit: Option<i64>,
    ) -> Result<RelayOutcome<OutboxPage>, StoreError>;
    fn ack_outbox(
        &self,
        now: i64,
        binding_id: &str,
        cursor: i64,
        provider_message_id: Option<&str>,
    ) -> Result<RelayOutcome<(i64, bool)>, StoreError>;
    fn claim_answer(
        &self,
        now: i64,
        binding_id: &str,
        reply_token: &str,
        answer: &str,
        answered_by: Option<&ExternalAuthor>,
    ) -> Result<RelayOutcome<AnswerClaim>, StoreError>;
    fn release_answer(&self, claim: &AnswerClaimRef) -> Result<bool, StoreError>;
    fn confirm_answer(&self, delivered: &AnswerDelivered) -> Result<bool, StoreError>;
    fn mark_prior_delivered(
        &self,
        claim: &AnswerClaimRef,
        prior: &PriorAnswer,
    ) -> Result<bool, StoreError>;
    fn close_question(
        &self,
        now: i64,
        session_durable_id: &str,
        ui_request_id: &str,
    ) -> Result<i64, StoreError>;
    fn register_incarnation(
        &self,
        durable_id: &str,
        incarnation: &str,
        endpoint_socket: Option<&str>,
        endpoint_kind: Option<&str>,
    ) -> Result<(), StoreError>;
    fn clear_endpoint(&self, durable_id: &str, incarnation: &str) -> Result<(), StoreError>;
    fn session_endpoint(
        &self,
        durable_id: &str,
    ) -> Result<Option<(String, String, String)>, StoreError>;

    fn register_store_extension(
        &self,
        descriptor: &StoreExtensionRegistration,
        now: i64,
        restored_at: Option<i64>,
    ) -> StoreExtensionResult<i64>;
    fn extension_call(
        &self,
        name: &str,
        op: &str,
        args: &Value,
        now: i64,
    ) -> StoreExtensionResult<Value>;
    fn session_callable_ops(&self) -> Vec<DeclaredSessionOp>;
    fn extension_session_await(
        &self,
        name: &str,
        op: &str,
        args: &Value,
        caller: &SessionCaller,
        now: i64,
    ) -> StoreExtensionResult<Value>;
    fn record_thread_creation(
        &self,
        record: &super::store_extension_facade::ThreadCreationRecord,
        now: i64,
    ) -> Result<(), StoreError>;
}

pub type SharedGatewayStore = Arc<dyn GatewayStoreApi>;
