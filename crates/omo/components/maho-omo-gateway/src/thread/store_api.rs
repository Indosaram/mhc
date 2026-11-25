//! The gateway store facade's API.
//!
//! Upstream counterpart: `thread/gateway/store-api.ts`. Every call is synchronous: the store owns one
//! connection and the worker protocol serializes calls onto it.

pub type BindingPageResult = Result<RelayOutcome<(Vec<BindingRecord>, Option<String>)>, StoreError>;
use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use super::backlog::TargetBacklog;
use super::session_wait_types::{
    DrainedOutcome, IntakeMark, IntakePair, OpenIntakeSession, QueuedWaitingOn, SessionWaitState,
    SessionWork, WriteSessionWaitRequest,
};
use super::store_extensions::{
    DeclaredSessionOp, SessionCaller, StoreExtensionRegistration, StoreExtensionResult,
};
use super::store_ops::{DeliveryReceipt, DeliveryView, StoreError};
use super::store_relay_ops::{
    AnswerClaim, AnswerClaimRef, AnswerDelivered, BindInput, BindingRecord, BindingsFilter,
    OutboxPage, PriorAnswer, RelayOutcome, ReportOpRequest, ReportOpResult, ToolReceiptBegin,
};
use super::types::{
    ClaimOutcome, ClaimRequest, DeliveryRow, EnqueueOutcome, EnqueueRequest, ExternalAuthor,
    GatewayStoreStats, IntakeOutcome, IntakeRequest, ProcessIdentity, ReconcileOutcome,
    ReconcileRequest, RecordOutcomeRequest, RefusalReason,
};
use super::woken_types::{
    WakeRequestOutcome, WakeRetryOutcome, WakeStartOutcome, WakeTakeOverOutcome, WokenSessionRow,
    WokenStateUpdate,
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
    fn intake(&self, request: &IntakeRequest) -> Result<IntakeOutcome, StoreError>;
    fn reconcile(&self, request: &ReconcileRequest) -> Result<ReconcileOutcome, StoreError>;
    fn claim(&self, request: &ClaimRequest) -> Result<ClaimOutcome, StoreError>;
    fn record_outcome(&self, request: &RecordOutcomeRequest) -> Result<ClaimOutcome, StoreError>;
    fn refuse_queued(
        &self,
        now: i64,
        delivery_id: &str,
        reason: RefusalReason,
    ) -> Result<bool, StoreError>;
    fn refuse_returned(
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
    fn backlogs(&self, targets: &[String]) -> Result<BTreeMap<String, TargetBacklog>, StoreError>;
    fn journal_mode(&self) -> Result<String, StoreError>;
    fn stats(&self) -> GatewayStoreStats;
    fn tool_receipt_begin(
        &self,
        now: i64,
        principal: &str,
        operation: &str,
        idempotency_key: &str,
        args_hash: &str,
    ) -> Result<ToolReceiptBegin, StoreError>;
    fn tool_receipt_settle(
        &self,
        now: i64,
        principal: &str,
        operation: &str,
        idempotency_key: &str,
        result_or_error: Result<&Value, &str>,
    ) -> Result<bool, StoreError>;
    fn tool_receipt_revise(
        &self,
        now: i64,
        principal: &str,
        operation: &str,
        idempotency_key: &str,
        result: &Value,
    ) -> Result<bool, StoreError>;
    fn tool_receipt_release(
        &self,
        principal: &str,
        operation: &str,
        idempotency_key: &str,
    ) -> Result<bool, StoreError>;

    fn woken_request(
        &self,
        durable_id: &str,
        session_file: &str,
    ) -> Result<WakeRequestOutcome, StoreError>;
    fn woken_start(&self, durable_id: &str) -> Result<WakeStartOutcome, StoreError>;
    fn woken_renew(
        &self,
        durable_id: &str,
        generation: i64,
    ) -> Result<Option<WokenSessionRow>, StoreError>;
    fn woken_take_over(&self, durable_id: &str) -> Result<WakeTakeOverOutcome, StoreError>;
    fn woken_set_state(
        &self,
        update: &WokenStateUpdate,
    ) -> Result<Option<WokenSessionRow>, StoreError>;
    fn woken_record_waiting(
        &self,
        durable_id: &str,
        generation: i64,
    ) -> Result<Option<WokenSessionRow>, StoreError>;
    fn woken_retry_later(
        &self,
        durable_id: &str,
        generation: i64,
    ) -> Result<WakeRetryOutcome, StoreError>;
    fn woken_clear_waiting(&self, durable_id: &str, generation: i64) -> Result<bool, StoreError>;
    fn woken_end(&self, durable_id: &str, generation: i64) -> Result<bool, StoreError>;
    fn woken_gated(&self, durable_id: &str) -> Result<bool, StoreError>;
    fn woken_list(&self) -> Result<Vec<WokenSessionRow>, StoreError>;
    fn woken_count_wake(
        &self,
        durable_id: &str,
        human_input_revision: Option<i64>,
    ) -> Result<i64, StoreError>;
    fn woken_wake_count(&self, durable_id: &str) -> Result<i64, StoreError>;
    fn woken_reset_count(&self, durable_id: &str) -> Result<(), StoreError>;
    fn woken_session_work(&self, durable_id: &str) -> Result<SessionWork, StoreError>;
    fn session_wait_write(&self, request: &WriteSessionWaitRequest) -> Result<(), StoreError>;
    fn session_wait_read(&self, durable_id: &str) -> Result<Option<SessionWaitState>, StoreError>;
    fn session_wait_clear(&self, durable_id: &str) -> Result<(), StoreError>;
    fn session_waiting_on(&self, durable_id: &str) -> Result<Option<QueuedWaitingOn>, StoreError>;
    fn intake_mark_record(
        &self,
        sender_device: &str,
        session_id: &str,
        command_id: &str,
        ledger_id: Option<&str>,
    ) -> Result<(), StoreError>;
    fn intake_mark_marked(&self) -> Result<Vec<IntakePair>, StoreError>;
    fn intake_mark_pending(&self) -> Result<Vec<IntakeMark>, StoreError>;
    fn intake_mark_drained(&self, pair: &IntakePair) -> Result<DrainedOutcome, StoreError>;
    fn intake_command_for(&self, ledger_id: &str) -> Result<Option<String>, StoreError>;
    fn intake_open_for_command(
        &self,
        sender_device: &str,
        command_id: &str,
    ) -> Result<bool, StoreError>;
    fn intake_open_sessions(&self) -> Result<Vec<OpenIntakeSession>, StoreError>;

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
