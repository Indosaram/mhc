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

/// What a host reads for an audit `action` or `code` it does not know, sent by a newer engine:
/// the event is kept, never dropped.
pub const UNKNOWN_AUDIT_VALUE: &str = "unknown";

/// The `"unknown"` audit value as a type (upstream `UnknownAuditValue`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum UnknownAuditValue {
    #[serde(rename = "unknown")]
    Unknown,
}

/// An audit `code`: an `ErrorCode` this host knows, or [`UNKNOWN_AUDIT_VALUE`] for one sent by a
/// newer engine (upstream `ErrorCode | UnknownAuditValue`). The host parser maps unrecognized codes
/// to `Unknown`; deserializing an unrecognized string directly is an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum AuditCode {
    Known(senpi_desktop_core::error::ErrorCode),
    Unknown(UnknownAuditValue),
}

impl AuditCode {
    pub const UNKNOWN: Self = Self::Unknown(UnknownAuditValue::Unknown);

    /// The wire string: the `ErrorCode` name or `"unknown"`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Known(code) => code.as_str(),
            Self::Unknown(_) => UNKNOWN_AUDIT_VALUE,
        }
    }
}

impl From<senpi_desktop_core::error::ErrorCode> for AuditCode {
    fn from(code: senpi_desktop_core::error::ErrorCode) -> Self {
        Self::Known(code)
    }
}

/// The `audit` notification payload: one per mutating request, success or failure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuditEvent {
    /// An `ENGINE_METHODS` name, or [`UNKNOWN_AUDIT_VALUE`] for one this host does not know.
    pub action: String,
    pub target: String,
    /// `background` or `foreground`.
    pub delivery: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_id: Option<String>,
    /// `None` when the request succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<AuditCode>,
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
