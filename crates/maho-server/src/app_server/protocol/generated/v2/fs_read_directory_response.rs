#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FsReadDirectoryResponse {
    #[serde(rename = "entries")]
    pub entries: Vec<Box<crate::app_server::protocol::generated::v2::fs_read_directory_entry::FsReadDirectoryEntry>>,
}
