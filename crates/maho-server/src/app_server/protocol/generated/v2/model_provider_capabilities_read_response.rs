#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelProviderCapabilitiesReadResponse {
    #[serde(rename = "namespaceTools")]
    pub namespace_tools: bool,
    #[serde(rename = "imageGeneration")]
    pub image_generation: bool,
    #[serde(rename = "webSearch")]
    pub web_search: bool,
}
