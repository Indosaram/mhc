#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarketplaceRemoveResponse {
    #[serde(rename = "marketplaceName")]
    pub marketplace_name: String,
    #[serde(rename = "installedRoot", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub installed_root: Option<Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>>,
}
