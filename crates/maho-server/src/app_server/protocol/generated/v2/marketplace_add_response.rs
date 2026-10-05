#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarketplaceAddResponse {
    #[serde(rename = "marketplaceName")]
    pub marketplace_name: String,
    #[serde(rename = "installedRoot")]
    pub installed_root: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "alreadyAdded")]
    pub already_added: bool,
}
