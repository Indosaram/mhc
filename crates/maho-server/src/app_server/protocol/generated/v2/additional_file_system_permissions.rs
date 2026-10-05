#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdditionalFileSystemPermissions {
    #[serde(rename = "read", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub read: Option<Vec<Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>>>,
    #[serde(rename = "write", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub write: Option<Vec<Box<crate::app_server::protocol::generated::legacy_app_path_string::LegacyAppPathString>>>,
    #[serde(rename = "globScanMaxDepth", default, skip_serializing_if = "Option::is_none")]
    pub glob_scan_max_depth: Option<f64>,
    #[serde(rename = "entries", default, skip_serializing_if = "Option::is_none")]
    pub entries: Option<Vec<Box<crate::app_server::protocol::generated::v2::file_system_sandbox_entry::FileSystemSandboxEntry>>>,
}
