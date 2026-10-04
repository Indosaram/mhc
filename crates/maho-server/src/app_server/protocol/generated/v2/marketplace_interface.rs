#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarketplaceInterface {
    #[serde(rename = "displayName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub display_name: Option<String>,
}
