#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarketplaceRemoveParams {
    #[serde(rename = "marketplaceName")]
    pub marketplace_name: String,
}
