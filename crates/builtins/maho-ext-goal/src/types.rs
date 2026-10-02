//! Wire-compatible goal records from goal/types.ts.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus { Active, Paused, Blocked, Complete }
impl std::fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self { Self::Active => "active", Self::Paused => "paused", Self::Blocked => "blocked", Self::Complete => "complete" })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelSettableGoalStatus { Complete, Blocked }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalAccountingMode { Active, ActiveOrBlocked, ActiveOrComplete }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalUpdateSource { Model, User }
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GoalStoreRef { pub base_dir: std::path::PathBuf, pub thread_id: String }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Goal {
    pub id: String,
    pub thread_id: String,
    pub objective: String,
    pub status: GoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub time_used_seconds: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consecutive_continuations: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unattended_continuations: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_continuation_signature: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoalFile { pub version: u8, pub goal: Option<Goal> }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageSnapshot { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_write: u64, pub total_tokens: u64 }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GoalUpdate { pub objective: Option<String>, pub status: Option<GoalStatus>, pub reason: Option<String>, pub token_budget: Option<Option<u64>> }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalToolSnapshot {
    pub thread_id: String, pub objective: String, pub status: GoalStatus,
    pub tokens_used: f64, pub time_used_seconds: f64, pub created_at: f64, pub updated_at: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocked_at: Option<f64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GoalToolResponse { pub goal: Option<GoalToolSnapshot> }
pub fn is_record(value: &serde_json::Value) -> bool { value.is_object() || value.is_array() }
