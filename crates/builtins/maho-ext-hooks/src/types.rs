use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SupportedHookEvent {
    PreToolUse,
    PostToolUse,
    UserPromptSubmit,
    SessionStart,
    PreCompact,
    PostCompact,
    Stop,
    Notification,
}

impl SupportedHookEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::SessionStart => "SessionStart",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::Stop => "Stop",
            Self::Notification => "Notification",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookSourceScope { Global, Project, Plugin, Runtime, Cli, Managed }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookDiscoveryTiming { PreSession, Runtime }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookSourceMetadata {
    pub scope: HookSourceScope,
    pub source_path: String,
    pub display_order: usize,
    pub discovered_at: HookDiscoveryTiming,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity { Error, Warning }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookDiagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    pub path: String,
    pub source: HookSourceMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}
