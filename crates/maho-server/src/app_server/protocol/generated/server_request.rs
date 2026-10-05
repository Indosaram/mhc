#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestItemCommandExecutionRequestApproval1 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::command_execution_request_approval_params::CommandExecutionRequestApprovalParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestItemFileChangeRequestApproval2 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::file_change_request_approval_params::FileChangeRequestApprovalParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestItemToolRequestUserInput3 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::tool_request_user_input_params::ToolRequestUserInputParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestMcpServerElicitationRequest4 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::mcp_server_elicitation_request_params::McpServerElicitationRequestParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestItemPermissionsRequestApproval5 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::permissions_request_approval_params::PermissionsRequestApprovalParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestItemToolCall6 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::dynamic_tool_call_params::DynamicToolCallParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestAccountChatgptAuthTokensRefresh7 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::chatgpt_auth_tokens_refresh_params::ChatgptAuthTokensRefreshParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestAttestationGenerate8 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::v2::attestation_generate_params::AttestationGenerateParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestApplyPatchApproval9 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::apply_patch_approval_params::ApplyPatchApprovalParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestExecCommandApproval10 {
    #[serde(rename = "id")]
    pub id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
    #[serde(rename = "params")]
    pub params: Box<crate::app_server::protocol::generated::exec_command_approval_params::ExecCommandApprovalParams>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "method")]
pub enum ServerRequest {
    #[serde(rename = "item/commandExecution/requestApproval")]
    ItemCommandExecutionRequestApproval(Box<ServerRequestItemCommandExecutionRequestApproval1>),
    #[serde(rename = "item/fileChange/requestApproval")]
    ItemFileChangeRequestApproval(Box<ServerRequestItemFileChangeRequestApproval2>),
    #[serde(rename = "item/tool/requestUserInput")]
    ItemToolRequestUserInput(Box<ServerRequestItemToolRequestUserInput3>),
    #[serde(rename = "mcpServer/elicitation/request")]
    McpServerElicitationRequest(Box<ServerRequestMcpServerElicitationRequest4>),
    #[serde(rename = "item/permissions/requestApproval")]
    ItemPermissionsRequestApproval(Box<ServerRequestItemPermissionsRequestApproval5>),
    #[serde(rename = "item/tool/call")]
    ItemToolCall(Box<ServerRequestItemToolCall6>),
    #[serde(rename = "account/chatgptAuthTokens/refresh")]
    AccountChatgptAuthTokensRefresh(Box<ServerRequestAccountChatgptAuthTokensRefresh7>),
    #[serde(rename = "attestation/generate")]
    AttestationGenerate(Box<ServerRequestAttestationGenerate8>),
    #[serde(rename = "applyPatchApproval")]
    ApplyPatchApproval(Box<ServerRequestApplyPatchApproval9>),
    #[serde(rename = "execCommandApproval")]
    ExecCommandApproval(Box<ServerRequestExecCommandApproval10>),
}
