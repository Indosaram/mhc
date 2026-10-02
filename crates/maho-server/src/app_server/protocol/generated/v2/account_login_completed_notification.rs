#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AccountLoginCompletedNotification {
    #[serde(rename = "loginId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub login_id: Option<String>,
    #[serde(rename = "success")]
    pub success: bool,
    #[serde(rename = "error", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub error: Option<String>,
}
