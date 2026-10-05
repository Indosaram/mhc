#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RawResponseCompletedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "responseId")]
    pub response_id: String,
    #[serde(rename = "usage", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub usage: Option<Box<crate::app_server::protocol::generated::v2::token_usage_breakdown::TokenUsageBreakdown>>,
}
