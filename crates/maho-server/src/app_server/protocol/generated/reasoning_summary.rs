#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReasoningSummary {
    #[serde(rename = "auto")]
    Auto,
    #[serde(rename = "concise")]
    Concise,
    #[serde(rename = "detailed")]
    Detailed,
    #[serde(rename = "none")]
    None,
}
