#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PermissionsRequestApprovalParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "environmentId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub environment_id: Option<String>,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: f64,
    #[serde(rename = "cwd")]
    pub cwd: Box<crate::app_server::protocol::generated::absolute_path_buf::AbsolutePathBuf>,
    #[serde(rename = "reason", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub reason: Option<String>,
    #[serde(rename = "permissions")]
    pub permissions: Box<crate::app_server::protocol::generated::v2::request_permission_profile::RequestPermissionProfile>,
}
