#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum HookEventName {
    #[serde(rename = "preToolUse")]
    PreToolUse,
    #[serde(rename = "permissionRequest")]
    PermissionRequest,
    #[serde(rename = "postToolUse")]
    PostToolUse,
    #[serde(rename = "preCompact")]
    PreCompact,
    #[serde(rename = "postCompact")]
    PostCompact,
    #[serde(rename = "sessionStart")]
    SessionStart,
    #[serde(rename = "sessionEnd")]
    SessionEnd,
    #[serde(rename = "userPromptSubmit")]
    UserPromptSubmit,
    #[serde(rename = "subagentStart")]
    SubagentStart,
    #[serde(rename = "subagentStop")]
    SubagentStop,
    #[serde(rename = "stop")]
    Stop,
}
