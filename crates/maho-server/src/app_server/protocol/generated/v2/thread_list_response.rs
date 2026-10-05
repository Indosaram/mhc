#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadListResponse {
    #[serde(rename = "data")]
    pub data: Vec<Box<crate::app_server::protocol::generated::v2::thread::Thread>>,
    #[serde(rename = "nextCursor", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub next_cursor: Option<String>,
    #[serde(rename = "backwardsCursor", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub backwards_cursor: Option<String>,
}
