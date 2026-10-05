#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadTokenUsage {
    #[serde(rename = "total")]
    pub total: Box<crate::app_server::protocol::generated::v2::token_usage_breakdown::TokenUsageBreakdown>,
    #[serde(rename = "last")]
    pub last: Box<crate::app_server::protocol::generated::v2::token_usage_breakdown::TokenUsageBreakdown>,
    #[serde(rename = "modelContextWindow", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_context_window: Option<f64>,
}
