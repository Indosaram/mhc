#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PermissionProfileListResponse {
    #[serde(rename = "data")]
    pub data: Vec<Box<crate::app_server::protocol::generated::v2::permission_profile_summary::PermissionProfileSummary>>,
    #[serde(rename = "nextCursor", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub next_cursor: Option<String>,
}
