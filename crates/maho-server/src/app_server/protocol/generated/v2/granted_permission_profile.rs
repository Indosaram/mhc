#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GrantedPermissionProfile {
    #[serde(rename = "network", default, skip_serializing_if = "Option::is_none")]
    pub network: Option<Box<crate::app_server::protocol::generated::v2::additional_network_permissions::AdditionalNetworkPermissions>>,
    #[serde(rename = "fileSystem", default, skip_serializing_if = "Option::is_none")]
    pub file_system: Option<Box<crate::app_server::protocol::generated::v2::additional_file_system_permissions::AdditionalFileSystemPermissions>>,
}
