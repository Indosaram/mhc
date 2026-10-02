#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentConfigImportItemTypeFailure {
    #[serde(rename = "itemType")]
    pub item_type: Box<crate::app_server::protocol::generated::v2::external_agent_config_migration_item_type::ExternalAgentConfigMigrationItemType>,
    #[serde(rename = "errorType", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub error_type: Option<String>,
    #[serde(rename = "subErrorType", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub sub_error_type: Option<String>,
    #[serde(rename = "failureStage")]
    pub failure_stage: String,
    #[serde(rename = "message")]
    pub message: String,
    #[serde(rename = "cwd", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub cwd: Option<String>,
    #[serde(rename = "source", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub source: Option<String>,
}
