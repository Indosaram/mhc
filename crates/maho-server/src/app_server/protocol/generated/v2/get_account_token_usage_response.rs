#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GetAccountTokenUsageResponse {
    #[serde(rename = "summary")]
    pub summary: Box<crate::app_server::protocol::generated::v2::account_token_usage_summary::AccountTokenUsageSummary>,
    #[serde(rename = "dailyUsageBuckets", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub daily_usage_buckets: Option<Vec<Box<crate::app_server::protocol::generated::v2::account_token_usage_daily_bucket::AccountTokenUsageDailyBucket>>>,
}
