#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AdditionalPermissionProfile {
    #[serde(rename = "network", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub network: Option<Box<crate::app_server::protocol::generated::v2::additional_network_permissions::AdditionalNetworkPermissions>>,
    #[serde(rename = "fileSystem", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub file_system: Option<Box<crate::app_server::protocol::generated::v2::additional_file_system_permissions::AdditionalFileSystemPermissions>>,
}
