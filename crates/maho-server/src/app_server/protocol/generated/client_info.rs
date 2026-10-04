#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClientInfo {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "title", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub title: Option<String>,
    #[serde(rename = "version")]
    pub version: String,
}
