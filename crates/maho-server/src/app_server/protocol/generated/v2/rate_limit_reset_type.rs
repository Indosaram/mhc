#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RateLimitResetType {
    #[serde(rename = "codexRateLimits")]
    CodexRateLimits,
    #[serde(rename = "unknown")]
    Unknown,
}
