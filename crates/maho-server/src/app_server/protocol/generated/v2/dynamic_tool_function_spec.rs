#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DynamicToolFunctionSpec {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "description")]
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
    #[serde(rename = "deferLoading", default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
}
