#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PluginSkillReadResponse {
    #[serde(rename = "contents", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub contents: Option<String>,
}
