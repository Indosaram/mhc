#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginMarketplaceEntry {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "path", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub path: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "interface", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub interface: Option<Box<crate::app_server::protocol::generated::v2::marketplace_interface::MarketplaceInterface>>,
    #[serde(rename = "plugins")]
    pub plugins: Vec<Box<crate::app_server::protocol::generated::v2::plugin_summary::PluginSummary>>,
}
