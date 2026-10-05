#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginListResponse {
    #[serde(rename = "marketplaces")]
    pub marketplaces: Vec<Box<crate::app_server::protocol::generated::v2::plugin_marketplace_entry::PluginMarketplaceEntry>>,
    #[serde(rename = "marketplaceLoadErrors")]
    pub marketplace_load_errors: Vec<Box<crate::app_server::protocol::generated::v2::marketplace_load_error_info::MarketplaceLoadErrorInfo>>,
    #[serde(rename = "featuredPluginIds")]
    pub featured_plugin_ids: Vec<String>,
}
