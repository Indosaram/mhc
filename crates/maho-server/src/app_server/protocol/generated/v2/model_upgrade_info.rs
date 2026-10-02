#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelUpgradeInfo {
    #[serde(rename = "model")]
    pub model: String,
    #[serde(rename = "upgradeCopy", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub upgrade_copy: Option<String>,
    #[serde(rename = "modelLink", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_link: Option<String>,
    #[serde(rename = "migrationMarkdown", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub migration_markdown: Option<String>,
}
