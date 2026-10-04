#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum AddCreditsNudgeCreditType {
    #[serde(rename = "credits")]
    Credits,
    #[serde(rename = "usage_limit")]
    UsageLimit,
}
