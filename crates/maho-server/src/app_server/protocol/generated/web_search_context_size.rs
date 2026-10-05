#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WebSearchContextSize {
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
}
