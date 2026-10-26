//! Session and run types mirrored from `src/session.ts`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::audit::AuditRecord;
use crate::wire::DesktopCapabilities;

/// Frozen run settings the host hands to the desktop runtime for one `computer` run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputerSessionSnapshot {
    pub cwd: String,
    pub session_id: String,
    pub capture_max_width: u32,
    pub capture_max_height: u32,
    pub capture_max_bytes: u64,
    pub display: String,
    pub read_only: bool,
    pub stop_hotkey: String,
    pub allow_host_relay_only_stop: bool,
}

/// Text shown to the model; structurally compatible with the ai package's `TextContent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputerTextDisplay {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

impl Default for ComputerTextDisplay {
    fn default() -> Self {
        Self {
            kind: "text".to_string(),
            text: String::new(),
        }
    }
}

/// Base64 image shown to the model; structurally compatible with the ai package's `ImageContent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputerImageDisplay {
    #[serde(rename = "type")]
    pub kind: String,
    pub data: String,
    pub mime_type: String,
}

impl Default for ComputerImageDisplay {
    fn default() -> Self {
        Self {
            kind: "image".to_string(),
            data: String::new(),
            mime_type: "image/png".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ComputerDisplay {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "image")]
    Image { data: String, #[serde(rename = "mimeType")] mime_type: String },
}

/// Full-resolution screenshot artifact written during one computer run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputerScreenshot {
    pub path: String,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_height: Option<u32>,
    pub target: String,
}

/// Successful computer run output returned to the tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ComputerRunOk {
    pub displays: Vec<ComputerDisplay>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub return_value: Option<Value>,
    pub screenshots: Vec<ComputerScreenshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<DesktopCapabilities>,
    pub audit: Vec<AuditRecord>,
}
