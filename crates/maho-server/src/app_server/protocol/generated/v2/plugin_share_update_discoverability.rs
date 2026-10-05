#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PluginShareUpdateDiscoverability {
    #[serde(rename = "UNLISTED")]
    UNLISTED,
    #[serde(rename = "PRIVATE")]
    PRIVATE,
    #[serde(rename = "LISTED")]
    LISTED,
}
