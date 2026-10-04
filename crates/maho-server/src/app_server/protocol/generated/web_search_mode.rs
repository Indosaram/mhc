#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum WebSearchMode {
    #[serde(rename = "disabled")]
    Disabled,
    #[serde(rename = "cached")]
    Cached,
    #[serde(rename = "indexed")]
    Indexed,
    #[serde(rename = "live")]
    Live,
}
