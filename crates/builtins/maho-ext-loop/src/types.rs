use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RequestedIntervalUnit { #[serde(rename="s")] Seconds, #[serde(rename="m")] Minutes, #[serde(rename="h")] Hours, #[serde(rename="d")] Days }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectiveIntervalUnit { #[serde(rename="m")] Minutes, #[serde(rename="h")] Hours, #[serde(rename="d")] Days }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestedInterval { pub value: f64, pub unit: RequestedIntervalUnit, pub raw: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct EffectiveInterval { pub value: f64, pub unit: EffectiveIntervalUnit, pub human: String, pub rounded: bool, pub rounding_notice: Option<String> }
pub type EpochMs = f64;
pub type LoopId = String;
pub type WakeupId = String;
pub type DeliveryId = String;
pub const LOOP_STATE_VERSION: u8 = 1;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum LoopKind { Fixed, Dynamic }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LoopSentinel { #[serde(rename="<<autonomous-loop>>")] Autonomous, #[serde(rename="<<autonomous-loop-dynamic>>")] AutonomousDynamic, #[serde(rename="<<loop.md>>")] LoopFile, #[serde(rename="<<loop.md-dynamic>>")] LoopFileDynamic }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag="type", rename_all="lowercase")]
pub enum LoopPayload { Prompt { prompt: String }, Sentinel { sentinel: LoopSentinel } }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum LoopPhase { Starting, Waiting, Queued, Running, Suspended, Ended }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="snake_case")]
pub enum LoopEndReason { Stopped, KeepaliveExhausted, Expired, TickBudgetExhausted, Error }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct LoopLifecycle { pub phase: LoopPhase, #[serde(skip_serializing_if="Option::is_none")] pub ended_at: Option<EpochMs>, #[serde(skip_serializing_if="Option::is_none")] pub end_reason: Option<LoopEndReason>, #[serde(skip_serializing_if="Option::is_none")] pub end_detail: Option<String> }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct LoopFileFingerprint { pub path: String, pub mtime_ms: f64, pub size: f64, pub content_hash: String, pub anchor_delivery_id: DeliveryId }
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SentinelDeliveryState { pub autonomous_preamble_delivered: bool, pub last_loop_file_delivered: Option<LoopFileFingerprint>, pub force_full_delivery: bool }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="kebab-case")]
pub enum LoopWakeSourceKind { TerminalMonitor, TerminalBackgroundSession, Task, Other }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct LoopWakeSource { pub source: LoopWakeSourceKind, pub id: String, #[serde(skip_serializing_if="Option::is_none")] pub description: Option<String>, pub created_at: EpochMs }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum WakeupSource { Model, Keepalive }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all="lowercase")]
pub enum DynamicKind { Dynamic }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct PendingWakeup { pub id: WakeupId, pub loop_id: LoopId, pub kind: DynamicKind, pub source: WakeupSource, pub requested_delay_seconds: f64, pub delay_seconds: f64, pub due_at: EpochMs, pub reason: String, pub prompt: String, pub noop: bool, pub created_at: EpochMs }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct LoopEntryFields { pub id: LoopId, pub original_args: String, pub reentry_prompt: String, pub payload: LoopPayload, pub created_at: EpochMs, pub last_fired_at: Option<EpochMs>, pub expires_at: EpochMs, pub last_scheduled_for_at: Option<EpochMs>, pub coalesced_fire_pending: bool, pub queued_for_at: Option<EpochMs>, pub noop_streak: f64, pub tick_count: f64, pub sentinel_delivery: SentinelDeliveryState, pub wake_sources: Vec<LoopWakeSource> }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag="kind", rename_all="lowercase")]
pub enum CronEntry {
    Fixed { #[serde(flatten)] fields: LoopEntryFields, #[serde(flatten)] lifecycle: LoopLifecycle, #[serde(rename="requestedInterval")] requested_interval: RequestedInterval, #[serde(rename="effectiveInterval")] effective_interval: EffectiveInterval, #[serde(rename="cronExpression")] cron_expression: String, #[serde(rename="nextFireAt")] next_fire_at: EpochMs, #[serde(rename="intervalMs")] interval_ms: f64 },
    Dynamic { #[serde(flatten)] fields: LoopEntryFields, #[serde(flatten)] lifecycle: LoopLifecycle, #[serde(rename="pendingWakeup")] pending_wakeup: Option<PendingWakeup>, #[serde(rename="keepaliveCredit")] keepalive_credit: u8 },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct LoopState { pub version: u8, pub session_id: String, pub entries: BTreeMap<LoopId, CronEntry>, pub active_dynamic_id: Option<LoopId>, pub updated_at: EpochMs }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopStoreRef { pub base_dir: PathBuf, pub session_id: String }
pub fn is_record(value: &serde_json::Value) -> bool { value.is_object() }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn sentinel_tokens_match_wire_contract() { let result = serde_json::to_value(LoopPayload::Sentinel { sentinel: LoopSentinel::LoopFileDynamic }).unwrap(); assert_eq!(result, serde_json::json!({"type":"sentinel","sentinel":"<<loop.md-dynamic>>"})); }
    #[test] fn empty_state_roundtrips() { let value = serde_json::json!({"version":1,"sessionId":"s","entries":{},"activeDynamicId":null,"updatedAt":1.0}); let state: LoopState = serde_json::from_value(value.clone()).unwrap(); let result = serde_json::to_value(state).unwrap(); assert_eq!(result, value); }
    #[test] fn record_excludes_arrays_and_null() { let result = [is_record(&serde_json::json!({})), is_record(&serde_json::json!([])), is_record(&serde_json::Value::Null)]; assert_eq!(result, [true, false, false]); }
}
