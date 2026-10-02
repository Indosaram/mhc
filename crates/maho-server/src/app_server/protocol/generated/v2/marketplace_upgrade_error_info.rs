#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarketplaceUpgradeErrorInfo {
    #[serde(rename = "marketplaceName")]
    pub marketplace_name: String,
    #[serde(rename = "message")]
    pub message: String,
}
