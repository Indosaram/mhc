#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FsGetMetadataParams {
    #[serde(rename = "path")]
    pub path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
}
