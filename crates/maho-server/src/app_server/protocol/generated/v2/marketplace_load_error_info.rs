#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MarketplaceLoadErrorInfo {
    #[serde(rename = "marketplacePath")]
    pub marketplace_path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "message")]
    pub message: String,
}
