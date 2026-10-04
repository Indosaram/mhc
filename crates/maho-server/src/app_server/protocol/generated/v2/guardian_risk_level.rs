#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GuardianRiskLevel {
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
    #[serde(rename = "critical")]
    Critical,
}
