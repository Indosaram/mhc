#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppsReadResponse {
    #[serde(rename = "apps")]
    pub apps: Vec<Box<crate::app_server::protocol::generated::v2::connector_metadata::ConnectorMetadata>>,
    #[serde(rename = "missingAppIds")]
    pub missing_app_ids: Vec<String>,
}
