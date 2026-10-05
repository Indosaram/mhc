#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadLoadedListResponse {
    #[serde(rename = "data")]
    pub data: Vec<String>,
    #[serde(rename = "nextCursor", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub next_cursor: Option<String>,
}
