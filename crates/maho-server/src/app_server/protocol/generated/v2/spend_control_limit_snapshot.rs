#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SpendControlLimitSnapshot {
    #[serde(rename = "limit")]
    pub limit: String,
    #[serde(rename = "used")]
    pub used: String,
    #[serde(rename = "remainingPercent")]
    pub remaining_percent: f64,
    #[serde(rename = "resetsAt")]
    pub resets_at: f64,
}
