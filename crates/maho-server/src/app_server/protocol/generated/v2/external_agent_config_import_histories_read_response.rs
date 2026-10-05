#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentConfigImportHistoriesReadResponse {
    #[serde(rename = "data")]
    pub data: Vec<Box<crate::app_server::protocol::generated::v2::external_agent_config_import_history::ExternalAgentConfigImportHistory>>,
    #[serde(rename = "connectors")]
    pub connectors: Vec<Box<crate::app_server::protocol::generated::v2::external_agent_imported_connector_candidate::ExternalAgentImportedConnectorCandidate>>,
}
