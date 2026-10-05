#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentConfigImportItemTypeSuccess {
    #[serde(rename = "itemType")]
    pub item_type: Box<crate::app_server::protocol::generated::v2::external_agent_config_migration_item_type::ExternalAgentConfigMigrationItemType>,
    #[serde(rename = "cwd", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub cwd: Option<String>,
    #[serde(rename = "source", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub source: Option<String>,
    #[serde(rename = "target", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub target: Option<String>,
}
