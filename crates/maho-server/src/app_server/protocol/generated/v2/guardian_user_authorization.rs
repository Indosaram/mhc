#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GuardianUserAuthorization {
    #[serde(rename = "unknown")]
    Unknown,
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
}
