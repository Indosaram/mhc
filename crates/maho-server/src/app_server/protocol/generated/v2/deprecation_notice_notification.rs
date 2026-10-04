#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeprecationNoticeNotification {
    #[serde(rename = "summary")]
    pub summary: String,
    #[serde(rename = "details", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub details: Option<String>,
}
