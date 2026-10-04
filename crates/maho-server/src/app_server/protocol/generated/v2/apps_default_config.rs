#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppsDefaultConfig {
    #[serde(rename = "enabled")]
    pub enabled: bool,
    #[serde(rename = "approvals_reviewer", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub approvals_reviewer: Option<Box<crate::app_server::protocol::generated::v2::approvals_reviewer::ApprovalsReviewer>>,
    #[serde(rename = "destructive_enabled")]
    pub destructive_enabled: bool,
    #[serde(rename = "open_world_enabled")]
    pub open_world_enabled: bool,
    #[serde(rename = "default_tools_approval_mode")]
    pub default_tools_approval_mode: Option<Box<crate::app_server::protocol::generated::v2::app_tool_approval::AppToolApproval>>,
}
