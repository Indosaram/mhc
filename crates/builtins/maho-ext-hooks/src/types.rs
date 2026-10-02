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

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag="event")]
pub enum HookInputWire {
    SessionStart {
        #[serde(rename="sessionId")] session_id:String,
        cwd:String,
        #[serde(skip_serializing_if="Option::is_none")] reason:Option<String>,
        #[serde(flatten)] fields:SessionHookFields,
    },
    UserPromptSubmit {prompt:String,cwd:String,#[serde(flatten)] fields:PromptHookFields},
    PreToolUse {
        #[serde(rename="toolName")] tool_name:String,
        #[serde(rename="toolInput")] tool_input:serde_json::Value,
        cwd:String,#[serde(flatten)] fields:PreToolHookFields,
    },
    PostToolUse {
        #[serde(rename="toolName")] tool_name:String,
        #[serde(rename="toolInput")] tool_input:serde_json::Value,
        #[serde(rename="toolOutput")] tool_output:serde_json::Value,
        cwd:String,#[serde(flatten)] fields:PostToolHookFields,
    },
    PreCompact {reason:String,cwd:String,#[serde(flatten)] fields:PreCompactHookFields},
    PostCompact {reason:String,cwd:String,#[serde(flatten)] fields:PostCompactHookFields},
    Stop {
        #[serde(rename="stopReason",skip_serializing_if="Option::is_none")] stop_reason:Option<String>,
        cwd:String,#[serde(flatten)] fields:SessionHookFields,
    },
    Notification {message:String,kind:String,cwd:String,#[serde(flatten)] fields:NotificationHookFields},
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionHookFields {
    #[serde(skip_serializing_if="Option::is_none")] pub hook_event_name:Option<SupportedHookEvent>,
    #[serde(skip_serializing_if="Option::is_none")] pub session_id:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub transcript_path:Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PromptHookFields {
    #[serde(skip_serializing_if="Option::is_none")] pub session_id:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub transcript_path:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub permission_mode:Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PreToolHookFields {
    #[serde(skip_serializing_if="Option::is_none")] pub hook_event_name:Option<SupportedHookEvent>,
    #[serde(skip_serializing_if="Option::is_none")] pub session_id:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub tool_name:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub tool_input:Option<serde_json::Value>,
    #[serde(skip_serializing_if="Option::is_none")] pub tool_use_id:Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PostToolHookFields {
    #[serde(flatten)] pub tool:PreToolHookFields,
    #[serde(skip_serializing_if="Option::is_none")] pub tool_response:Option<serde_json::Value>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PreCompactHookFields {
    #[serde(flatten)] pub session:SessionHookFields,
    #[serde(skip_serializing_if="Option::is_none")] pub custom_instructions:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub request_id:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub will_retry:Option<bool>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PostCompactHookFields {
    #[serde(flatten)] pub session:SessionHookFields,
    #[serde(skip_serializing_if="Option::is_none")] pub request_id:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub will_retry:Option<bool>,
    #[serde(skip_serializing_if="Option::is_none")] pub accepted:Option<bool>,
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NotificationHookFields {
    #[serde(flatten)] pub session:SessionHookFields,
    #[serde(skip_serializing_if="Option::is_none")] pub title:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub notification_source:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub request_id:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub status:Option<String>,
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    #[test]
    fn tool_wire_preserves_aliases_and_arbitrary_tool_values() {
        let input=serde_json::json!({"event":"PreToolUse","cwd":"/repo","toolName":"bash","toolInput":{"command":"pwd"},"tool_name":"bash","tool_input":{"command":"pwd"},"tool_use_id":"call","session_id":"session","hook_event_name":"PreToolUse"});
        let parsed:HookInputWire=serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(),input);
        assert!(serde_json::from_value::<HookInputWire>(serde_json::json!({"event":"PreToolUse","cwd":"/repo"})).is_err());
    }
    #[test]
    fn lifecycle_builders_round_trip_optional_fields_without_duplicate_reason() {
        use crate::lifecycle_adapter::*;
        let context=LifecycleInputContext {cwd:"/repo",session_id:"session",transcript_path:Some("/session.jsonl")};
        for input in [build_session_start_hook_input("startup",&context),build_pre_compact_hook_input("manual","request",true,Some("brief"),&context),build_post_compact_hook_input("manual","request",false,true,&context),build_notification_hook_input(NotificationHookInput {message:"ready",kind:"tool",title:Some("title"),source:Some("monitor"),request_id:Some("request"),status:Some("completed")},&context)] {
            let wire:HookInputWire=serde_json::from_value(input.clone()).unwrap();assert_eq!(serde_json::to_value(wire).unwrap(),input);
        }
    }
}
