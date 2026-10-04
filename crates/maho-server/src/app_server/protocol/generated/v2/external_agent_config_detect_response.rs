#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentConfigDetectResponse {
    #[serde(rename = "items")]
    pub items: Vec<Box<crate::app_server::protocol::generated::v2::external_agent_config_migration_item::ExternalAgentConfigMigrationItem>>,
}
