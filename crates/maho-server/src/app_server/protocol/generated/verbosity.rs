#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Verbosity {
    #[serde(rename = "low")]
    Low,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "high")]
    High,
}
