use memory_core::reflection::machine::{DreamOrigin, ReflectionTrigger};
use serde::{Serialize, Deserialize};
pub const REFLECTION_COMPLETION_ENTRY_TYPE: &str = "senpi-memory.reflection-completion";
pub const REFLECTION_LAUNCHED_ENTRY_TYPE: &str = "senpi-memory.reflection-launched";
pub const REFLECTION_SUMMARY_ENTRY_TYPE: &str = "senpi-memory.reflection-summary";
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionCompletionSummary { pub schema_version: u32, pub count: usize, pub failed_count: usize, #[serde(rename = "oldestISO")] pub oldest_iso: String, #[serde(rename = "newestISO")] pub newest_iso: String, pub dominant_fingerprint: String }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionLaunchedEntry {
    pub schema_version: u32, pub run_id: String, pub identity: String, pub trigger: ReflectionTrigger,
    pub category: String, #[serde(skip_serializing_if = "Option::is_none")] pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub thinking: Option<String>,
    pub conversation_ids: Vec<String>, pub backlog_steps: usize, pub started_at: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryStatus { Pending, Consumed }
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionDelivery {
    pub status: DeliveryStatus,
    #[serde(skip_serializing_if = "Option::is_none")] pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub consumed_at: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReflectionCompletionRecord {
    pub schema_version: u32, pub run_id: String, pub identity: String, pub category: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub thinking: Option<String>,
    pub conversation_ids: Vec<String>, pub trigger: ReflectionTrigger,
    #[serde(skip_serializing_if = "Option::is_none")] pub origin: Option<DreamOrigin>,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub detail: Option<String>,
    pub started_at: String, pub finished_at: String,
    #[serde(skip_serializing_if = "Option::is_none")] pub duration_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub merged_commit_sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub files_changed: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")] pub consecutive_failures: Option<usize>,
    pub delivery: CompletionDelivery,
}
