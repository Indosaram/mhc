#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FsCopyParams {
    #[serde(rename = "sourcePath")]
    pub source_path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "destinationPath")]
    pub destination_path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "recursive", default, skip_serializing_if = "Option::is_none")]
    pub recursive: Option<bool>,
}
