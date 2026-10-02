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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin_env: Option<std::collections::BTreeMap<String,String>>,
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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandHookConfig {
    #[serde(rename = "type")]
    pub kind: String,
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_windows: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_message: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableHookHandler {
    pub event: SupportedHookEvent,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub group_index: usize,
    pub handler_index: usize,
    pub config: CommandHookConfig,
    pub source: HookSourceMetadata,
}

#[derive(Debug, Default)]
pub struct ParsedHookConfig {
    pub executable_handlers: Vec<ExecutableHookHandler>,
    pub diagnostics: Vec<HookDiagnostic>,
}

pub const UNSUPPORTED_KNOWN_HOOK_EVENTS: &[&str] = &[
    "PermissionRequest", "PermissionDenied", "SubagentStart", "SubagentStop", "Setup",
    "UserPromptExpansion", "PostToolUseFailure", "PostToolBatch", "TaskCreated", "TaskCompleted",
    "StopFailure", "TeammateIdle", "InstructionsLoaded", "ConfigChange", "CwdChanged", "FileChanged",
    "WorktreeCreate", "WorktreeRemove", "MessageDisplay", "SessionEnd", "Elicitation", "ElicitationResult",
];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookTrustEntry {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trusted_hash: Option<String>,
    pub scope: HookSourceScope,
    pub source_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    pub command_preview: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct HookTrustState {
    pub version: u8,
    pub hooks: std::collections::BTreeMap<String, HookTrustEntry>,
}
