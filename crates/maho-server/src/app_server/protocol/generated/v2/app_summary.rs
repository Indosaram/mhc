#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppSummary {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "description", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub description: Option<String>,
    #[serde(rename = "installUrl", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub install_url: Option<String>,
    #[serde(rename = "category", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub category: Option<String>,
}
