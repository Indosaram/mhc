//! Gateway delivery types: provenance, envelopes, rows and store requests.
//!
//! Upstream counterpart: `thread/gateway/types.ts`. New deliveries always carry provenance; a JSON
//! envelope written before provenance was recorded stays readable and is never attributed to the
//! receiver.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::errors::ThreadToolFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayDeliveryMode {
    Auto,
    Steer,
    FollowUp,
}

impl GatewayDeliveryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "steer" => Some(Self::Steer),
            "follow_up" => Some(Self::FollowUp),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Queued,
    Admitting,
    Admitted,
    Applied,
    Refused,
    Uncertain,
}

impl DeliveryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Admitting => "admitting",
            Self::Admitted => "admitted",
            Self::Applied => "applied",
            Self::Refused => "refused",
            Self::Uncertain => "uncertain",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "admitting" => Some(Self::Admitting),
            "admitted" => Some(Self::Admitted),
            "applied" => Some(Self::Applied),
            "refused" => Some(Self::Refused),
            "uncertain" => Some(Self::Uncertain),
            _ => None,
        }
    }

    pub fn is_open(self) -> bool {
        matches!(self, Self::Queued | Self::Admitting | Self::Admitted)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveLane {
    Steer,
    FollowUp,
}

impl EffectiveLane {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    NotSteerable,
    TurnConflict,
    Expired,
    BindingClosed,
}

impl RefusalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotSteerable => "not_steerable",
            Self::TurnConflict => "turn_conflict",
            Self::Expired => "expired",
            Self::BindingClosed => "binding_closed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "not_steerable" => Some(Self::NotSteerable),
            "turn_conflict" => Some(Self::TurnConflict),
            "expired" => Some(Self::Expired),
            "binding_closed" => Some(Self::BindingClosed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessIdentity {
    pub pid: i64,
    pub process_start_time: Option<String>,
    pub instance_id: String,
    /// The host generation the claiming runtime runs in; null in a terminal.
    pub runtime_instance: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalAuthor {
    pub platform_user_id: String,
    pub display: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalOrigin {
    pub platform: String,
    pub account_id: String,
    pub chat_id: String,
    pub thread_id: String,
    pub message_id: String,
    /// Absent on a message sent without an author, and on every row written before authors existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<ExternalAuthor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeOrigin {
    Session { session: String },
    External { external: ExternalOrigin },
}

impl EnvelopeOrigin {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Session { session } => Some(session),
            Self::External { .. } => None,
        }
    }

    pub fn external(&self) -> Option<&ExternalOrigin> {
        match self {
            Self::Session { .. } => None,
            Self::External { external } => Some(external),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub workspace: String,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lane: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryEnvelope {
    pub provenance: Provenance,
    pub origin: EnvelopeOrigin,
    pub actor: String,
    pub via: Vec<String>,
    pub root_id: String,
    pub hop: i64,
    pub delivery_id: String,
}

/// JSON written before provenance was recorded remains readable, never attributed to the receiver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDeliveryEnvelope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
    pub origin: EnvelopeOrigin,
    pub actor: String,
    pub via: Vec<String>,
    pub root_id: String,
    pub hop: i64,
    pub delivery_id: String,
}

impl StoredDeliveryEnvelope {
    pub fn from_envelope(envelope: DeliveryEnvelope) -> Self {
        Self {
            provenance: Some(envelope.provenance),
            origin: envelope.origin,
            actor: envelope.actor,
            via: envelope.via,
            root_id: envelope.root_id,
            hop: envelope.hop,
            delivery_id: envelope.delivery_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryRow {
    pub delivery_id: String,
    pub target_durable_id: String,
    pub seq: i64,
    pub sender: String,
    pub sender_turn: Option<String>,
    pub envelope: StoredDeliveryEnvelope,
    pub body: String,
    pub bytes: i64,
    pub mode_requested: GatewayDeliveryMode,
    pub mode_effective: Option<EffectiveLane>,
    pub expected_turn_id: Option<i64>,
    pub state: DeliveryState,
    pub reason: Option<String>,
    pub admitted_by: Option<ProcessIdentity>,
    pub claimed_at: Option<i64>,
    pub attempt: i64,
    pub admission_kind: Option<String>,
    pub turn_epoch: Option<i64>,
    pub root_id: String,
    pub hop: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub expires_at: i64,
    pub binding_id: Option<String>,
    pub binding_revision: Option<i64>,
    pub actor_user_id: Option<String>,
}

/// Who is sending. Every kind maps to one receipt/budget principal and one causal-graph node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewaySender {
    Session {
        durable_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cause_delivery_id: Option<String>,
    },
    Cli {
        uid: i64,
        user: String,
    },
    External {
        origin: ExternalOrigin,
        binding_id: String,
        binding_revision: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingRef {
    pub binding_id: String,
    pub revision: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptRef {
    pub idempotency_key: String,
    pub args_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayEndpointKind {
    RpcHost,
    Tui,
}

impl GatewayEndpointKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RpcHost => "rpc_host",
            Self::Tui => "tui",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnqueueRequest {
    pub provenance: Provenance,
    pub now: i64,
    pub delivery_id: String,
    pub target_durable_id: String,
    pub sender_principal: String,
    /// The pair rate bucket's sender key; `sender_principal` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_principal: Option<String>,
    pub sender_node: String,
    pub sender_turn: Option<String>,
    pub cause_delivery_id: Option<String>,
    pub claimed_root_id: Option<String>,
    pub origin: EnvelopeOrigin,
    pub actor: String,
    pub body: String,
    pub mode: GatewayDeliveryMode,
    pub expected_turn_id: Option<i64>,
    pub binding: Option<BindingRef>,
    pub receipt: ReceiptRef,
    pub endpoint_kind: Option<GatewayEndpointKind>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoreRefusal {
    pub code: super::errors::ThreadErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnqueueOutcome {
    Inserted { row: Box<DeliveryRow>, queue_position: i64 },
    Replay { result: Value },
    Refused { code: super::errors::ThreadErrorCode, message: String, details: Option<Value> },
    Busy,
}

impl EnqueueOutcome {
    pub fn refused(code: &str, message: impl Into<String>) -> Self {
        Self::Refused {
            code: super::errors::ThreadErrorCode::parse(code).unwrap_or_else(|| panic!("unknown thread error code: {code}")),
            message: message.into(),
            details: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionLedger {
    pub pending: Vec<String>,
    pub emitted: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileRequest {
    pub now: i64,
    pub target_durable_id: String,
    pub self_identity: ProcessIdentity,
    pub ledger: AdmissionLedger,
    pub session_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryTransition {
    pub delivery_id: String,
    pub from: DeliveryState,
    pub to: DeliveryState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileOutcome {
    pub queued: Vec<DeliveryRow>,
    pub transitions: Vec<DeliveryTransition>,
    pub dual_runtime: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimRequest {
    pub now: i64,
    pub delivery_id: String,
    pub self_identity: ProcessIdentity,
    pub lane: EffectiveLane,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ClaimOutcome {
    Claimed { row: Box<DeliveryRow> },
    Lost { state: Option<DeliveryState> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutcomeRecord {
    Admitted { admission_kind: String, turn_epoch: i64 },
    Applied { admission_kind: String, turn_epoch: i64 },
    Requeue,
    Refused { reason: RefusalReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordOutcomeRequest {
    pub now: i64,
    pub delivery_id: String,
    pub self_identity: ProcessIdentity,
    pub outcome: OutcomeRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewayDeliveryDelivery {
    Started { turn_id: String },
    Steered { turn_id: String },
    Queued { queue_position: i64 },
    QueuedOffline { queue_position: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayDeliverySuccess {
    pub thread_id: String,
    pub delivery_id: String,
    pub message_seq: i64,
    pub delivery: GatewayDeliveryDelivery,
    pub effective_mode: String,
    pub endpoint_kind: Option<GatewayEndpointKind>,
    pub deduplicated: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewayDeliveryResult {
    Ok {
        thread_id: String,
        delivery_id: String,
        message_seq: i64,
        delivery: GatewayDeliveryDelivery,
        effective_mode: String,
        endpoint_kind: Option<GatewayEndpointKind>,
        deduplicated: bool,
    },
    Error { error: ThreadToolFailure },
}

/// A legacy mailbox item that can never be delivered; it is reported, not imported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyMailboxSkip {
    pub message_seq: i64,
    pub target: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GatewayStoreEvent {
    ExtensionError { extension: String, phase: String, error: String },
    Busy { op: String },
    LockWaitExceeded { op: String, waited_ms: i64 },
    CompletionsEmitted { session_durable_id: String, cursors: Vec<i64> },
    Paused { hook: String },
    Barrier { op: String },
    LegacyMailboxInvalid { directory: String, error: String },
    LegacyMailboxSkipped { directory: String, items: Vec<LegacyMailboxSkip> },
    StoreClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayTestHookAction {
    Pause,
    Sigkill,
    Throw,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayStoreTestHooks {
    pub before_db_commit: Option<GatewayTestHookAction>,
    pub after_db_commit: Option<GatewayTestHookAction>,
    pub announce_barrier: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayStoreConfig {
    pub agent_dir: String,
    pub busy_timeout_ms: u64,
    pub lock_wait_max_ms: u64,
    pub instance_id: String,
    pub runtime_instance: Option<String>,
    pub legacy_mailbox_directories: Vec<String>,
    pub test_hooks: GatewayStoreTestHooks,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayStoreStats {
    pub writes: i64,
    pub marker_unlinks: i64,
    pub transactions: i64,
}
