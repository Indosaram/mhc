#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileChangeRequestApprovalResponse {
    #[serde(rename = "decision")]
    pub decision: Box<crate::app_server::protocol::generated::v2::file_change_approval_decision::FileChangeApprovalDecision>,
}
