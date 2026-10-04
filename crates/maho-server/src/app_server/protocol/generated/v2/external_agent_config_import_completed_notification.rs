#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentConfigImportCompletedNotification {
    #[serde(rename = "importId")]
    pub import_id: String,
    #[serde(rename = "itemTypeResults")]
    pub item_type_results: Vec<Box<crate::app_server::protocol::generated::v2::external_agent_config_import_type_result::ExternalAgentConfigImportTypeResult>>,
}
