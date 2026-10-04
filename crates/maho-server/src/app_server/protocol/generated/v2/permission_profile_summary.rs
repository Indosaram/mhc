#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PermissionProfileSummary {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "description", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub description: Option<String>,
    #[serde(rename = "allowed")]
    pub allowed: bool,
}
