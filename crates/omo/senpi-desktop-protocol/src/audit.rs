//! Audit types mirrored from `src/audit.ts`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::engine_schema_generated::ErrorCode;

/// Outcome of one audited desktop action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AuditStatus {
    Success,
    Error,
    Suspended,
}

/// The first failure of an audited action, as reported to the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditPrimaryError {
    pub code: ErrorCode,
    pub message: String,
}

/// One persisted audit line: the engine `AuditEvent` plus run context. FROZEN field set;
/// the engine audit writer persists exactly these fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditRecord {
    /// ISO-8601 UTC timestamp.
    pub timestamp: String,
    pub session_id: String,
    pub run_id: String,
    pub action: String,
    pub target: String,
    /// `background` or `foreground`.
    pub delivery: String,
    pub frame_id: Option<String>,
    /// `None` when the action succeeded.
    pub code: Option<ErrorCode>,
    pub status: AuditStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_error: Option<AuditPrimaryError>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_restored: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screenshot_height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_length: Option<u32>,
    /// First 16 hex digits of the typed text's SHA-256; the text itself is never audited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}
