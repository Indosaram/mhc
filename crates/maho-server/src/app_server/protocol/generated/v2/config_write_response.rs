#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConfigWriteResponse {
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::write_status::WriteStatus>,
    #[serde(rename = "version")]
    pub version: String,
    #[serde(rename = "filePath")]
    pub file_path: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "overriddenMetadata", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub overridden_metadata: Option<Box<crate::app_server::protocol::generated::v2::overridden_metadata::OverriddenMetadata>>,
}
