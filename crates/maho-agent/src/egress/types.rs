//! Port of senpi `packages/agent/src/egress/types.ts`.

use maho_ai::types::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::types::StreamFn;

pub type ProviderId = String;
pub type DeviceId = String;
pub type Epoch = u64;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EgressRoute {
    Direct,
    Sealed,
    Device,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RouteReason {
    #[serde(rename = "fused")]
    Fused,
    #[serde(rename = "custody:sealed")]
    CustodySealed,
    #[serde(rename = "custody:device")]
    CustodyDevice,
    #[serde(rename = "roster:unknown")]
    RosterUnknown,
    #[serde(rename = "custody:cwd-scoped")]
    CustodyCwdScoped,
    #[serde(rename = "custody:unavailable")]
    CustodyUnavailable,
    #[serde(rename = "owner:revoked")]
    OwnerRevoked,
    #[serde(rename = "definition:variant")]
    DefinitionVariant,
    #[serde(rename = "address:private")]
    AddressPrivate,
    #[serde(rename = "address:self")]
    AddressSelf,
    #[serde(rename = "address:unknown")]
    AddressUnknown,
    #[serde(rename = "capability:local-only")]
    CapabilityLocalOnly,
    #[serde(rename = "capability:ambient")]
    CapabilityAmbient,
    #[serde(rename = "capability:callback-stream")]
    CapabilityCallbackStream,
    #[serde(rename = "capability:callback-stream-undefined")]
    CapabilityCallbackStreamUndefined,
    #[serde(rename = "api:no-fetch-injection")]
    ApiNoFetchInjection,
    #[serde(rename = "network:device-bound")]
    NetworkDeviceBound,
    #[serde(rename = "provider:extension")]
    ProviderExtension,
    #[serde(rename = "provider:hand-local")]
    ProviderHandLocal,
    #[serde(rename = "provider:unknown")]
    ProviderUnknown,
    #[serde(rename = "hook:stream")]
    HookStream,
    #[serde(rename = "hook:stream-undefined")]
    HookStreamUndefined,
    #[serde(rename = "hook:unknown")]
    HookUnknown,
    #[serde(rename = "hook:request-offline")]
    HookRequestOffline,
    #[serde(rename = "redirect:cross-origin")]
    RedirectCrossOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum WaitCondition {
    #[serde(rename = "device-online")]
    DeviceOnline { device_id: DeviceId },
    #[serde(rename = "custody-available")]
    CustodyAvailable,
    #[serde(rename = "refresh-ack")]
    RefreshAck { provider_id: ProviderId },
    #[serde(rename = "roster-sync")]
    RosterSync,
    #[serde(rename = "hand-selected")]
    HandSelected,
    #[serde(rename = "hand-advertised")]
    HandAdvertised { device_id: DeviceId },
    #[serde(rename = "config-head-change")]
    ConfigHeadChange,
    #[serde(rename = "live-digest-change")]
    LiveDigestChange {
        device_id: DeviceId,
        provider_id: ProviderId,
    },
    #[serde(rename = "storage-available")]
    StorageAvailable,
    #[serde(rename = "epoch-fresh")]
    EpochFresh,
    #[serde(rename = "claim-renewed")]
    ClaimRenewed,
    #[serde(rename = "user-choice")]
    UserChoice { choice_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutorHooksState {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressRouteDecision {
    pub route: EgressRoute,
    pub reason: RouteReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<DeviceId>,
    pub served_now: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_for: Option<Vec<WaitCondition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_hooks_on: Option<DeviceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_hooks: Option<ExecutorHooksState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serve_own_definition: Option<bool>,
}

/// The durable admission fence; `generation` is the agent instance generation, and older generations are refused for good.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionFence {
    pub epoch: Epoch,
    pub admission_seq: u64,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepRef {
    pub session_id: String,
    pub step_id: String,
    pub attempt: u64,
    pub fence: AdmissionFence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuxRef {
    pub session_id: String,
    pub aux_id: String,
    pub aux_key: String,
    pub stream_kind: String,
    pub fence: AdmissionFence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum CallRef {
    Step(StepRef),
    Aux(AuxRef),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EgressWaitReason {
    Route(RouteReason),
    #[serde(rename = "device-offline")]
    DeviceOffline,
    #[serde(rename = "result-pending")]
    ResultPending,
    #[serde(rename = "refresh-pending")]
    RefreshPending,
    #[serde(rename = "config-churn")]
    ConfigChurn,
    #[serde(rename = "epoch-stale")]
    EpochStale,
    #[serde(rename = "claim-expired")]
    ClaimExpired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EgressWait {
    pub call: CallRef,
    pub reason: String,
    pub steer: String,
    pub wait_for: Vec<WaitCondition>,
}

pub trait EgressHostHooks: Send + Sync {
    fn current_step(&self) -> StepRef;
    fn admit_aux(&self, stream_kind: &str, aux_key: &str) -> BoxFuture<'_, Result<AuxRef, String>>;
    fn on_wait(&self, wait: EgressWait) -> BoxFuture<'_, Result<(), String>>;
    fn on_wait_end(&self, call_id: &str) -> BoxFuture<'_, Result<(), String>>;
    fn history_has_step(&self, step_id: &str) -> BoxFuture<'_, Result<bool, String>>;
}

/// Bound from a hosted session latching on a failed step commit to its rebuilt instance accepting work.
pub const HOSTED_REBUILD_DEADLINE_MS: i64 = 5_000;
/// Maximum age of a registry epoch check accepted for a secret release, a step commit or a tool dispatch.
pub const EPOCH_FRESHNESS_MS: i64 = 5_000;
/// Margin a new placement owner waits beyond `EPOCH_FRESHNESS_MS` before cutting over a moved session.
pub const MOVE_SKEW_MS: i64 = 2_000;

pub trait ModelEgress: Send + Sync {
    /// Pure and cheap: no I/O, safe to call from UI code.
    fn route_for(&self, provider_id: &str) -> EgressRouteDecision;
    fn stream(&self) -> StreamFn;
}
