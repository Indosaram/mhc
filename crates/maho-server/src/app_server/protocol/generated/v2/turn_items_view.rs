#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TurnItemsView {
    #[serde(rename = "notLoaded")]
    NotLoaded,
    #[serde(rename = "summary")]
    Summary,
    #[serde(rename = "full")]
    Full,
}
