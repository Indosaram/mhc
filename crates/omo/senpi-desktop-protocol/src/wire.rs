//! Wire and capability types mirrored from `src/wire.ts`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub use senpi_desktop_core::protocol_results::{LogLevel, StopPathKind, StopPathStatus};
pub use senpi_desktop_core::types::DesktopCapabilities;

/// The `engine.log` notification payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngineLog {
    pub level: LogLevel,
    pub message: String,
}

/// The `audit` notification payload: one per mutating request, success or failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    pub action: String,
    pub target: String,
    /// `background` or `foreground`.
    pub delivery: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    /// `None` when the request succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<senpi_desktop_core::error::ErrorCode>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus_restored: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_length: Option<u32>,
    /// Fully delivered Unicode scalars, including when typing was interrupted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_delivered: Option<u32>,
    /// First 16 hex digits of the typed text's SHA-256; the text itself is never audited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
}
