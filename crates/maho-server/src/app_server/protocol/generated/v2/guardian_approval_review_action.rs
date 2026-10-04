#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuardianApprovalReviewActionCommand1 {
    #[serde(rename = "source")]
    pub source: Box<crate::app_server::protocol::generated::v2::guardian_command_source::GuardianCommandSource>,
    #[serde(rename = "command")]
    pub command: String,
    #[serde(rename = "cwd")]
    pub cwd: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuardianApprovalReviewActionExecve2 {
    #[serde(rename = "source")]
    pub source: Box<crate::app_server::protocol::generated::v2::guardian_command_source::GuardianCommandSource>,
    #[serde(rename = "program")]
    pub program: String,
    #[serde(rename = "argv")]
    pub argv: Vec<String>,
    #[serde(rename = "cwd")]
    pub cwd: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuardianApprovalReviewActionApplyPatch3 {
    #[serde(rename = "cwd")]
    pub cwd: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "files")]
    pub files: Vec<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuardianApprovalReviewActionNetworkAccess4 {
    #[serde(rename = "target")]
    pub target: String,
    #[serde(rename = "host")]
    pub host: String,
    #[serde(rename = "protocol")]
    pub protocol: Box<crate::app_server::protocol::generated::v2::network_approval_protocol::NetworkApprovalProtocol>,
    #[serde(rename = "port")]
    pub port: f64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuardianApprovalReviewActionMcpToolCall5 {
    #[serde(rename = "server")]
    pub server: String,
    #[serde(rename = "toolName")]
    pub tool_name: String,
    #[serde(rename = "connectorId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub connector_id: Option<String>,
    #[serde(rename = "connectorName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub connector_name: Option<String>,
    #[serde(rename = "toolTitle", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub tool_title: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GuardianApprovalReviewActionRequestPermissions6 {
    #[serde(rename = "reason", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub reason: Option<String>,
    #[serde(rename = "permissions")]
    pub permissions: Box<crate::app_server::protocol::generated::v2::request_permission_profile::RequestPermissionProfile>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum GuardianApprovalReviewAction {
    #[serde(rename = "command")]
    Command(Box<GuardianApprovalReviewActionCommand1>),
    #[serde(rename = "execve")]
    Execve(Box<GuardianApprovalReviewActionExecve2>),
    #[serde(rename = "applyPatch")]
    ApplyPatch(Box<GuardianApprovalReviewActionApplyPatch3>),
    #[serde(rename = "networkAccess")]
    NetworkAccess(Box<GuardianApprovalReviewActionNetworkAccess4>),
    #[serde(rename = "mcpToolCall")]
    McpToolCall(Box<GuardianApprovalReviewActionMcpToolCall5>),
    #[serde(rename = "requestPermissions")]
    RequestPermissions(Box<GuardianApprovalReviewActionRequestPermissions6>),
}
