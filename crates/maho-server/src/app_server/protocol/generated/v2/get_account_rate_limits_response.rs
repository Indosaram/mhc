pub type GetAccountRateLimitsResponseRateLimitsByLimitId1 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::v2::rate_limit_snapshot::RateLimitSnapshot>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetAccountRateLimitsResponse {
    #[serde(rename = "rateLimits")]
    pub rate_limits: Box<crate::app_server::protocol::generated::v2::rate_limit_snapshot::RateLimitSnapshot>,
    #[serde(rename = "rateLimitsByLimitId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub rate_limits_by_limit_id: Option<GetAccountRateLimitsResponseRateLimitsByLimitId1>,
    #[serde(rename = "rateLimitResetCredits", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub rate_limit_reset_credits: Option<Box<crate::app_server::protocol::generated::v2::rate_limit_reset_credits_summary::RateLimitResetCreditsSummary>>,
}
