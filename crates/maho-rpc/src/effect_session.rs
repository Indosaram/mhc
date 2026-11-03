//! The session seam the effect ledger and the client-admission ledger read and write (the parts of senpi's
//! `AgentSession` that `rpc/client-admissions.ts`, `rpc/effect-ledger.ts` and `rpc/effect-commands.ts`
//! touch). The ledgers own no session: a host adapter implements [`EffectSession`] over its loaded
//! session and forwards the session events the ledgers observe as [`EffectSessionEvent`] values.
//!
//! The adapter over `maho_core::agent_session::AgentSession` lives with the host that owns the session;
//! every method here names the senpi member it stands for.
use crate::client_admission_record::PreparedAdmissionInput;
use futures::future::BoxFuture;
use serde_json::Value;

/// One queued input as `getQueuedInputs()` reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedInputView {
    pub queued_id: String,
    pub client_message_id: Option<String>,
    pub text: String,
    pub revision: u64,
}

/// senpi `changeQueuedInput` result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuedChangeStatus {
    Removed,
    Edited,
    AlreadyDelivered,
    NotFound,
    Conflict,
}
impl QueuedChangeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Removed => "removed",
            Self::Edited => "edited",
            Self::AlreadyDelivered => "already_delivered",
            Self::NotFound => "not_found",
            Self::Conflict => "conflict",
        }
    }
}

/// Why `setModel` refused: senpi `ModelUsabilityBudgetError`, `ModelSwitchAuthError`, or anything else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetModelError {
    ContextBudget(String),
    ProviderUnavailable(String),
    Other(String),
}

/// senpi `model_change_cancelled.reason`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelChangeCancelReason {
    ContextBudget,
    Error,
    Superseded,
}

/// The session events the ledgers react to, already narrowed by the adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectSessionEvent {
    /// Agent-level `message_start` of a user message, with the identity stamped on it.
    UserMessageStart {
        client_message_id: Option<String>,
    },
    AgentStart,
    /// `agent_end`; `assistant_error` is `Some` when the last message is an assistant `stopReason: "error"`,
    /// carrying its `errorMessage`.
    AgentEnd {
        will_retry: bool,
        assistant_error: Option<Option<String>>,
    },
    AgentIdle,
    /// `queue_update`, reduced to the `clientMessageId` of every still-queued input.
    QueueUpdate {
        queued_ids: Vec<Option<String>>,
    },
    ModelChanged {
        effect_id: Option<String>,
    },
    ModelChangeCancelled {
        effect_id: Option<String>,
        reason: ModelChangeCancelReason,
        detail: Option<String>,
    },
}

pub trait EffectSession: Send + Sync {
    /// `sessionManager.getSessionId()`.
    fn durable_session_id(&self) -> String;
    /// The routing key `note_session_effect` publishes under (senpi keys the `AgentSession` object).
    fn effect_route(&self) -> String;
    /// `sessionManager.getEntries()`.
    fn entries(&self) -> Vec<Value>;
    /// `sessionManager.appendCustomEntry(customType, data)`.
    fn append_custom_entry(&self, custom_type: &str, data: Value);
    /// `sessionManager.persistHeaderNow()`.
    fn persist_header_now(&self) -> BoxFuture<'_, Result<(), String>>;
    /// `sessionFile`.
    fn session_file(&self) -> Option<String>;
    /// `isStreaming`.
    fn is_streaming(&self) -> bool;
    /// `currentAbortSource`.
    fn current_abort_source(&self) -> Option<String>;
    /// `clientMessageId` of `agent.state.streamingMessage`.
    fn streaming_client_message_id(&self) -> Option<String>;
    /// `restoreQueuedInput(prepared)`.
    fn restore_queued_input(
        &self,
        prepared: PreparedAdmissionInput,
    ) -> BoxFuture<'_, Result<(), String>>;
    /// `clearQueue({ abortWillFollow })`: the number of steering plus follow-up inputs drained.
    fn clear_queue(&self, abort_will_follow: bool) -> usize;
    /// `getQueuedInputs()`.
    fn queued_inputs(&self) -> Vec<QueuedInputView>;
    /// `changeQueuedInput(queuedId, text, { expectedRevision, effectId })`; `text: None` removes it.
    fn change_queued_input(
        &self,
        queued_id: &str,
        text: Option<String>,
        expected_revision: Option<u64>,
        effect_id: Option<String>,
    ) -> BoxFuture<'_, Result<QueuedChangeStatus, String>>;
    /// `modelRegistry.getAvailable()` lookup: the model as answered on the wire, or `None`.
    fn find_available_model(
        &self,
        provider: &str,
        model_id: &str,
    ) -> BoxFuture<'_, Result<Option<Value>, String>>;
    /// `setModel(model, { source: "rpc", effectId })`: the new `systemPromptName`, if any.
    fn set_model(
        &self,
        provider: &str,
        model_id: &str,
        effect_id: Option<String>,
    ) -> BoxFuture<'_, Result<Option<String>, SetModelError>>;
    /// `getAvailableThinkingLevels()`.
    fn available_thinking_levels(&self) -> Vec<String>;
    /// `setSessionThinkingLevel` when `turn_scope`, else `setThinkingLevel`, with `{ source: "rpc", effectId }`.
    fn set_thinking_level(&self, level: &str, turn_scope: bool, effect_id: Option<String>);
}
