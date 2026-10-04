#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PluginShareDiscoverability {
    #[serde(rename = "LISTED")]
    LISTED,
    #[serde(rename = "UNLISTED")]
    UNLISTED,
    #[serde(rename = "PRIVATE")]
    PRIVATE,
}
