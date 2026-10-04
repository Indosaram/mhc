#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ManagedHooksRequirements {
    #[serde(rename = "managedDir", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub managed_dir: Option<String>,
    #[serde(rename = "windowsManagedDir", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub windows_managed_dir: Option<String>,
    #[serde(rename = "PreToolUse")]
    pub pre_tool_use: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
    #[serde(rename = "PermissionRequest")]
    pub permission_request: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
    #[serde(rename = "PostToolUse")]
    pub post_tool_use: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
    #[serde(rename = "PreCompact")]
    pub pre_compact: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
    #[serde(rename = "PostCompact")]
    pub post_compact: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
    #[serde(rename = "SessionStart")]
    pub session_start: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
    #[serde(rename = "SessionEnd")]
    pub session_end: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
    #[serde(rename = "UserPromptSubmit")]
    pub user_prompt_submit: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
    #[serde(rename = "SubagentStart")]
    pub subagent_start: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
    #[serde(rename = "SubagentStop")]
    pub subagent_stop: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
    #[serde(rename = "Stop")]
    pub stop: Vec<Box<crate::app_server::protocol::generated::v2::configured_hook_matcher_group::ConfiguredHookMatcherGroup>>,
}
