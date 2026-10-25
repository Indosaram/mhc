//! Extension contracts corresponding to senpi core/extensions/types.ts.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageItem {
    pub item_id: String,
    pub producer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_ref: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageReceipt {
    pub item_id: String,
    pub entry_id: String,
    pub disposition: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice_id: Option<String>,
}
#[derive(Clone)]
pub struct SessionMoveExportEvent {
    pub move_id: String,
    pub dir: std::path::PathBuf,
    pub signal: maho_ext_api::AbortSignal,
}
#[derive(Clone)]
pub struct SessionMoveImportEvent {
    pub move_id: String,
    pub dir: std::path::PathBuf,
    pub redelivered: bool,
    pub signal: maho_ext_api::AbortSignal,
}
pub use crate::lifecycle_children::LifecycleChildren;
pub use crate::lifecycle_ledger::LifecycleLedger;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolBarrierSpec {
    pub id: String,
    pub deadline_ms: u64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolWorkspaceEffect {
    Read,
    Write,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSizeEntry {
    pub name: String,
    pub source: String,
    pub bytes: i64,
    pub estimated_tokens: i64,
    pub bounding_settings: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewritten: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<PromptSizeEntry>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_tools: Option<Vec<Value>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptSizeReport {
    pub sections: Vec<PromptSizeEntry>,
    pub total: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset_parts_mismatch: Option<Value>,
    #[serde(flatten)]
    pub startup_notes: serde_json::Map<String, Value>,
}
