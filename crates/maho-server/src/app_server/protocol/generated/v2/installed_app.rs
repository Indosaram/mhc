#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InstalledApp {
    #[serde(rename = "id")]
    pub id: String,
    #[serde(rename = "runtimeName", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub runtime_name: Option<String>,
    #[serde(rename = "enabled")]
    pub enabled: bool,
    #[serde(rename = "callable")]
    pub callable: bool,
}
