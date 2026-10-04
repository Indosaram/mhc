#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReviewDelivery {
    #[serde(rename = "inline")]
    Inline,
    #[serde(rename = "detached")]
    Detached,
}
