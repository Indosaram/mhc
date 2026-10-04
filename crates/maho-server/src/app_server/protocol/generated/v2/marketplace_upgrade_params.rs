#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarketplaceUpgradeParams {
    #[serde(rename = "marketplaceName", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub marketplace_name: Option<Option<String>>,
}
