#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConsumeAccountRateLimitResetCreditResponse {
    #[serde(rename = "outcome")]
    pub outcome: Box<crate::app_server::protocol::generated::v2::consume_account_rate_limit_reset_credit_outcome::ConsumeAccountRateLimitResetCreditOutcome>,
}
