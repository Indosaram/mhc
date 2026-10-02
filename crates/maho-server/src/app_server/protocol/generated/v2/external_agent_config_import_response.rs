#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExternalAgentConfigImportResponse {
    #[serde(rename = "importId")]
    pub import_id: String,
}
