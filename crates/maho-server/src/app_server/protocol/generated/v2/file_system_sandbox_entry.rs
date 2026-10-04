#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileSystemSandboxEntry {
    #[serde(rename = "path")]
    pub path: Box<crate::app_server::protocol::generated::v2::file_system_path::FileSystemPath>,
    #[serde(rename = "access")]
    pub access: Box<crate::app_server::protocol::generated::v2::file_system_access_mode::FileSystemAccessMode>,
}
