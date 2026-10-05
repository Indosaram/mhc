#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RateLimitResetCreditStatus {
    #[serde(rename = "available")]
    Available,
    #[serde(rename = "redeeming")]
    Redeeming,
    #[serde(rename = "redeemed")]
    Redeemed,
    #[serde(rename = "unknown")]
    Unknown,
}
