#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AppsReadParams {
    #[serde(rename = "appIds")]
    pub app_ids: Vec<String>,
    #[serde(rename = "includeTools", default, skip_serializing_if = "Option::is_none")]
    pub include_tools: Option<bool>,
}
