#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FsWriteFileParams {
    #[serde(rename = "path")]
    pub path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "dataBase64")]
    pub data_base64: String,
}
