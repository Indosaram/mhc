#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentImportedConnectorCandidate {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "sessionCount")]
    pub session_count: f64,
    #[serde(rename = "source")]
    pub source: Box<crate::app_server::protocol::generated::v2::external_agent_imported_connector_source::ExternalAgentImportedConnectorSource>,
}
