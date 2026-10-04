#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CommandExecutionRequestApprovalResponse {
    #[serde(rename = "decision")]
    pub decision: Box<crate::app_server::protocol::generated::v2::command_execution_approval_decision::CommandExecutionApprovalDecision>,
}
