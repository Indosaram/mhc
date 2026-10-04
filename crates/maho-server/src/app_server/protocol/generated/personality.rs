#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Personality {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "friendly")]
    Friendly,
    #[serde(rename = "pragmatic")]
    Pragmatic,
}
