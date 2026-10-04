#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileChangeRequestApprovalParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "startedAtMs")]
    pub started_at_ms: f64,
    #[serde(rename = "reason", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub reason: Option<Option<String>>,
    #[serde(rename = "grantRoot", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub grant_root: Option<Option<String>>,
}
