#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadSettings {
    #[serde(rename = "cwd")]
    pub cwd: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "approvalPolicy")]
    pub approval_policy: Box<crate::app_server::protocol::generated::v2::ask_for_approval::AskForApproval>,
    #[serde(rename = "approvalsReviewer")]
    pub approvals_reviewer: Box<crate::app_server::protocol::generated::v2::approvals_reviewer::ApprovalsReviewer>,
    #[serde(rename = "sandboxPolicy")]
    pub sandbox_policy: Box<crate::app_server::protocol::generated::v2::sandbox_policy::SandboxPolicy>,
    #[serde(rename = "activePermissionProfile", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub active_permission_profile: Option<Box<crate::app_server::protocol::generated::v2::active_permission_profile::ActivePermissionProfile>>,
    #[serde(rename = "model")]
    pub model: String,
    #[serde(rename = "modelProvider")]
    pub model_provider: String,
    #[serde(rename = "serviceTier", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub service_tier: Option<String>,
    #[serde(rename = "effort", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub effort: Option<Box<crate::app_server::protocol::generated::reasoning_effort::ReasoningEffort>>,
    #[serde(rename = "summary", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub summary: Option<Box<crate::app_server::protocol::generated::reasoning_summary::ReasoningSummary>>,
    #[serde(rename = "collaborationMode")]
    pub collaboration_mode: Box<crate::app_server::protocol::generated::collaboration_mode::CollaborationMode>,
    #[serde(rename = "personality", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub personality: Option<Box<crate::app_server::protocol::generated::personality::Personality>>,
}
