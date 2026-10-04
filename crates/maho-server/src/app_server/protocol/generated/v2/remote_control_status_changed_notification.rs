#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RemoteControlStatusChangedNotification {
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::remote_control_connection_status::RemoteControlConnectionStatus>,
    #[serde(rename = "serverName")]
    pub server_name: String,
    #[serde(rename = "installationId")]
    pub installation_id: String,
    #[serde(rename = "environmentId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub environment_id: Option<String>,
}
