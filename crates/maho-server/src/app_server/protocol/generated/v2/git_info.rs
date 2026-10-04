#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GitInfo {
    #[serde(rename = "sha", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub sha: Option<String>,
    #[serde(rename = "branch", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub branch: Option<String>,
    #[serde(rename = "originUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub origin_url: Option<String>,
}
