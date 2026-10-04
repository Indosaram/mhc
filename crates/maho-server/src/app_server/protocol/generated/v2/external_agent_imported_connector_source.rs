#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ExternalAgentImportedConnectorSource {
    #[serde(rename = "remoteMcpServersConfig")]
    Value,
}
