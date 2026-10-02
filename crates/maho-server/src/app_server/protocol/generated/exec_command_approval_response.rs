#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExecCommandApprovalResponse {
    #[serde(rename = "decision")]
    pub decision: Box<crate::app_server::protocol::generated::review_decision::ReviewDecision>,
}
