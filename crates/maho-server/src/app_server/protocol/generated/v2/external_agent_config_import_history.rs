#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentConfigImportHistory {
    #[serde(rename = "importId")]
    pub import_id: String,
    #[serde(rename = "completedAtMs")]
    pub completed_at_ms: i64,
    #[serde(rename = "successes")]
    pub successes: Vec<Box<crate::app_server::protocol::generated::v2::external_agent_config_import_item_type_success::ExternalAgentConfigImportItemTypeSuccess>>,
    #[serde(rename = "failures")]
    pub failures: Vec<Box<crate::app_server::protocol::generated::v2::external_agent_config_import_item_type_failure::ExternalAgentConfigImportItemTypeFailure>>,
}
