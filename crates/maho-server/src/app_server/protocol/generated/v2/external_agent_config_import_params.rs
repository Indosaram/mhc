#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentConfigImportParams {
    #[serde(rename = "migrationItems")]
    pub migration_items: Vec<Box<crate::app_server::protocol::generated::v2::external_agent_config_migration_item::ExternalAgentConfigMigrationItem>>,
    #[serde(rename = "source", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub source: Option<Option<String>>,
    #[serde(rename = "providerId", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub provider_id: Option<Option<String>>,
    #[serde(rename = "migrationSource", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub migration_source: Option<Option<String>>,
}
