#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RateLimitResetCreditsSummary {
    #[serde(rename = "availableCount")]
    pub available_count: i64,
    #[serde(rename = "credits", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub credits: Option<Vec<Box<crate::app_server::protocol::generated::v2::rate_limit_reset_credit::RateLimitResetCredit>>>,
}
