//! The narrow seam between the gateway and the host.
//!
//! Upstream counterpart: `thread/gateway/adapter.ts`. Every host interaction the store, engine and
//! drain need goes through these two ports: the sender side (`GatewayEndpointPort`) and the
//! receiver side (`SessionRuntimePort`).

pub type ReleaseSessionCallback = Option<Arc<dyn Fn(GatewayEndpointRef, ReleaseSessionRequest) -> BoxFuture<ReleaseSessionReply> + Send + Sync>>;
pub type RespondUiCallback = Option<Arc<dyn Fn(GatewayEndpointRef, UiAnswer) -> BoxFuture<UiAnswerReply> + Send + Sync>>;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::{AdmissionLedger, GatewayDeliveryMode, GatewayEndpointKind};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointLiveness {
    Routable,
    LiveUnresponsive,
    Dead,
}

impl EndpointLiveness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Routable => "routable",
            Self::LiveUnresponsive => "live_unresponsive",
            Self::Dead => "dead",
        }
    }
}

/// An endpoint that serves (or last served) a session. `routing_id` is null when the endpoint names
/// its session only by durable id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayEndpointRef {
    pub kind: GatewayEndpointKind,
    pub socket: String,
    pub routing_id: Option<String>,
    pub durable_session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExternalAdmissionKind {
    Started,
    Queued,
    Steered,
    TurnConflict,
    HeldDraft,
    AlreadyAdmitted,
}

impl ExternalAdmissionKind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "started" => Some(Self::Started),
            "queued" => Some(Self::Queued),
            "steered" => Some(Self::Steered),
            "turn_conflict" => Some(Self::TurnConflict),
            "held_draft" => Some(Self::HeldDraft),
            "already_admitted" => Some(Self::AlreadyAdmitted),
            _ => None,
        }
    }
}

/// The drain reports two omo-only outcomes the host has no member for: `not_steerable` and `expired`
/// are refusals decided by the gateway before the runtime was asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayAdmissionKind {
    Started,
    Queued,
    Steered,
    TurnConflict,
    HeldDraft,
    AlreadyAdmitted,
    NotSteerable,
    Expired,
}

impl GatewayAdmissionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Queued => "queued",
            Self::Steered => "steered",
            Self::TurnConflict => "turn_conflict",
            Self::HeldDraft => "held_draft",
            Self::AlreadyAdmitted => "already_admitted",
            Self::NotSteerable => "not_steerable",
            Self::Expired => "expired",
        }
    }

    pub fn from_external(kind: ExternalAdmissionKind) -> Self {
        match kind {
            ExternalAdmissionKind::Started => Self::Started,
            ExternalAdmissionKind::Queued => Self::Queued,
            ExternalAdmissionKind::Steered => Self::Steered,
            ExternalAdmissionKind::TurnConflict => Self::TurnConflict,
            ExternalAdmissionKind::HeldDraft => Self::HeldDraft,
            ExternalAdmissionKind::AlreadyAdmitted => Self::AlreadyAdmitted,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Admission {
    pub delivery_id: String,
    pub kind: ExternalAdmissionKind,
}

/// `unroutable`: the endpoint names its session only by durable id and its host lists no live
/// session under it, so no wake was sent; the delivery stays queued for the session's next start.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayWakeReply {
    #[serde(default)]
    pub admitted: Vec<Admission>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unroutable: Option<String>,
}

impl GatewayWakeReply {
    pub fn unroutable_no_live_session() -> Self {
        Self { admitted: Vec::new(), unroutable: Some("no_live_session".to_owned()) }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseDropped {
    #[serde(default)]
    pub deliveries: Vec<String>,
    #[serde(default)]
    pub user_messages: Vec<String>,
}

/// The host's `release_session` request; any reason but `takeover` is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseSessionRequest {
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupt: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseErrorData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub busy: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dropped: Option<ReleaseDropped>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_with: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseSuccessData {
    pub released: bool,
    pub session_path: String,
    pub attachments: i64,
    pub dropped: ReleaseDropped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "success")]
pub enum ReleaseSessionReply {
    #[serde(rename = "true")]
    Success { data: ReleaseSuccessData },
    #[serde(rename = "false")]
    Failure { error: String, #[serde(default, skip_serializing_if = "Option::is_none")] error_data: Option<ReleaseErrorData> },
}

impl ReleaseSessionReply {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success { .. })
    }
}

/// `fields` is the answer in the shape its request kind reads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UiAnswer {
    pub ui_request_id: String,
    pub fields: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "delivered")]
pub enum UiAnswerReply {
    #[serde(rename = "true")]
    Delivered,
    #[serde(rename = "false")]
    Refused { error: String },
}

/// Sender side: reaching another session's endpoint.
#[derive(Clone)]
pub struct GatewayEndpointPort {
    pub wake: Arc<dyn Fn(GatewayEndpointRef, Vec<String>) -> BoxFuture<GatewayWakeReply> + Send + Sync>,
    pub release_session: ReleaseSessionCallback,
    pub classify_liveness: Option<Arc<dyn Fn(GatewayEndpointRef) -> BoxFuture<EndpointLiveness> + Send + Sync>>,
    pub respond_ui: RespondUiCallback,
}

impl GatewayEndpointPort {
    /// A port that announces nothing: the shape an extension transaction uses, where a wake would
    /// reach a receiver before COMMIT.
    pub fn inert() -> Self {
        Self {
            wake: Arc::new(|_, _| Box::pin(async { GatewayWakeReply { admitted: Vec::new(), unroutable: Some("extension_transaction".into()) } })),
            release_session: None,
            classify_liveness: None,
            respond_ui: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimePhase {
    Idle,
    MidTurn,
    WaitingQuestion,
    Compacting,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAdmissionGate {
    pub can_admit: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold_reason: Option<String>,
    pub editor_revision: i64,
    pub turn_epoch: i64,
}

/// Who sent a delivery, for the surfaces a human reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliverySender {
    Agent { session_id: String, #[serde(default, skip_serializing_if = "Option::is_none")] name: Option<String> },
    CommandLine { #[serde(default, skip_serializing_if = "Option::is_none")] user: Option<String> },
    External { platform: String, #[serde(default, skip_serializing_if = "Option::is_none")] author: Option<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmitExternalMessageInput {
    pub delivery_id: String,
    pub text: String,
    pub deliver_as: GatewayDeliveryMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_turn_id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sender: Option<DeliverySender>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmitResult {
    pub kind: ExternalAdmissionKind,
    pub turn_epoch: i64,
}

/// Receiver side: the target session's own runtime, called from its drain.
#[derive(Clone)]
pub struct SessionRuntimePort {
    pub phase: Arc<dyn Fn() -> RuntimePhase + Send + Sync>,
    pub admission_gate: Arc<dyn Fn() -> SessionAdmissionGate + Send + Sync>,
    pub admit_external_message: Arc<dyn Fn(AdmitExternalMessageInput) -> Result<AdmitResult, String> + Send + Sync>,
    pub list_admitted_deliveries: Arc<dyn Fn() -> AdmissionLedger + Send + Sync>,
}

/// The host's wake reason plus the drain-internal `start` (first pass after registration).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeReason {
    Idle,
    Submission,
    DraftCleared,
    Command,
    Inbox,
    Emitted,
    Continue,
    Start,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainWakeEvent {
    pub reason: WakeReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_ids: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionControlDrainResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted: Option<Vec<Admission>>,
}

/// The host's `admitted` list names what the runtime took; the gateway's own refusals stay on the row.
pub fn to_session_control_drain_result(admitted: &[(String, GatewayAdmissionKind)]) -> SessionControlDrainResult {
    let mut taken = Vec::new();
    for (delivery_id, kind) in admitted {
        if matches!(kind, GatewayAdmissionKind::NotSteerable | GatewayAdmissionKind::Expired) {
            continue;
        }
        let external = match kind {
            GatewayAdmissionKind::Started => ExternalAdmissionKind::Started,
            GatewayAdmissionKind::Queued => ExternalAdmissionKind::Queued,
            GatewayAdmissionKind::Steered => ExternalAdmissionKind::Steered,
            GatewayAdmissionKind::TurnConflict => ExternalAdmissionKind::TurnConflict,
            GatewayAdmissionKind::HeldDraft => ExternalAdmissionKind::HeldDraft,
            GatewayAdmissionKind::AlreadyAdmitted => ExternalAdmissionKind::AlreadyAdmitted,
            GatewayAdmissionKind::NotSteerable | GatewayAdmissionKind::Expired => continue,
        };
        taken.push(Admission { delivery_id: delivery_id.clone(), kind: external });
    }
    SessionControlDrainResult { admitted: Some(taken) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gateway_only_refusals_are_left_out_of_the_host_result() {
        let admitted = vec![
            ("a".to_owned(), GatewayAdmissionKind::Started),
            ("b".to_owned(), GatewayAdmissionKind::NotSteerable),
            ("c".to_owned(), GatewayAdmissionKind::Expired),
            ("d".to_owned(), GatewayAdmissionKind::Queued),
        ];
        let result = to_session_control_drain_result(&admitted);
        let admitted = result.admitted.unwrap_or_default();
        let ids: Vec<&str> = admitted.iter().map(|entry| entry.delivery_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "d"]);
    }

    #[test]
    fn an_unroutable_wake_names_its_reason() {
        let reply = GatewayWakeReply::unroutable_no_live_session();
        assert_eq!(reply.unroutable.as_deref(), Some("no_live_session"));
        assert!(reply.admitted.is_empty());
    }
}
