#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadStartResponse {
    #[serde(rename = "thread")]
    pub thread: Box<crate::app_server::protocol::generated::v2::thread::Thread>,
    #[serde(rename = "model")]
    pub model: String,
    #[serde(rename = "modelProvider")]
    pub model_provider: String,
    #[serde(rename = "serviceTier", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub service_tier: Option<String>,
    #[serde(rename = "cwd")]
    pub cwd: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "instructionSources")]
    pub instruction_sources: Vec<Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>>,
    #[serde(rename = "approvalPolicy")]
    pub approval_policy: Box<crate::app_server::protocol::generated::v2::ask_for_approval::AskForApproval>,
    #[serde(rename = "approvalsReviewer")]
    pub approvals_reviewer: Box<crate::app_server::protocol::generated::v2::approvals_reviewer::ApprovalsReviewer>,
    #[serde(rename = "sandbox")]
    pub sandbox: Box<crate::app_server::protocol::generated::v2::sandbox_policy::SandboxPolicy>,
    #[serde(rename = "reasoningEffort", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub reasoning_effort: Option<Box<crate::app_server::protocol::generated::reasoning_effort::ReasoningEffort>>,
}
