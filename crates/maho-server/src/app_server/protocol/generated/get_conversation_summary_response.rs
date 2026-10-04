#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetConversationSummaryResponse {
    #[serde(rename = "summary")]
    pub summary: Box<crate::app_server::protocol::generated::conversation_summary::ConversationSummary>,
}
