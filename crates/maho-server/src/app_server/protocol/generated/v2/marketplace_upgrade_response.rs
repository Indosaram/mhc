#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarketplaceUpgradeResponse {
    #[serde(rename = "selectedMarketplaces")]
    pub selected_marketplaces: Vec<String>,
    #[serde(rename = "upgradedRoots")]
    pub upgraded_roots: Vec<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
    #[serde(rename = "errors")]
    pub errors: Vec<Box<crate::app_server::protocol::generated::v2::marketplace_upgrade_error_info::MarketplaceUpgradeErrorInfo>>,
}
