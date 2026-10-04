#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdditionalNetworkPermissions {
    #[serde(rename = "enabled", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub enabled: Option<bool>,
}
