#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecCommandApprovalParams {
    #[serde(rename = "conversationId")]
    pub conversation_id: Box<crate::app_server::protocol::generated::thread_id::ThreadId>,
    #[serde(rename = "callId")]
    pub call_id: String,
    #[serde(rename = "approvalId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub approval_id: Option<String>,
    #[serde(rename = "command")]
    pub command: Vec<String>,
    #[serde(rename = "cwd")]
    pub cwd: String,
    #[serde(rename = "reason", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub reason: Option<String>,
    #[serde(rename = "parsedCmd")]
    pub parsed_cmd: Vec<Box<crate::app_server::protocol::generated::parsed_command::ParsedCommand>>,
}
