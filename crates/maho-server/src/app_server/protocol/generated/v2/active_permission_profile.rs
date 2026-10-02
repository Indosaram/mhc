#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActivePermissionProfile {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "extends", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub extends: Option<String>,
}
