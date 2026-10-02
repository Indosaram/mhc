#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccountRateLimitsUpdatedNotification {
    #[serde(rename = "rateLimits")]
    pub rate_limits: Box<crate::app_server::protocol::generated::v2::rate_limit_snapshot::RateLimitSnapshot>,
}
