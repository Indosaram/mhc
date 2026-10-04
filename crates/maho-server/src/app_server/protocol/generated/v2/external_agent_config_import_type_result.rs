#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentConfigImportTypeResult {
    #[serde(rename = "itemType")]
    pub item_type: Box<crate::app_server::protocol::generated::v2::external_agent_config_migration_item_type::ExternalAgentConfigMigrationItemType>,
    #[serde(rename = "successes")]
    pub successes: Vec<Box<crate::app_server::protocol::generated::v2::external_agent_config_import_item_type_success::ExternalAgentConfigImportItemTypeSuccess>>,
    #[serde(rename = "failures")]
    pub failures: Vec<Box<crate::app_server::protocol::generated::v2::external_agent_config_import_item_type_failure::ExternalAgentConfigImportItemTypeFailure>>,
}
