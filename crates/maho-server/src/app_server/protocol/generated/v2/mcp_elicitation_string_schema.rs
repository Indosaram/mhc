#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpElicitationStringSchema {
    #[serde(rename = "type")]
    pub r#type: Box<crate::app_server::protocol::generated::v2::mcp_elicitation_string_type::McpElicitationStringType>,
    #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "minLength", default, skip_serializing_if = "Option::is_none")]
    pub min_length: Option<f64>,
    #[serde(rename = "maxLength", default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<f64>,
    #[serde(rename = "format", default, skip_serializing_if = "Option::is_none")]
    pub format: Option<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_string_format::McpElicitationStringFormat>>,
    #[serde(rename = "default", default, skip_serializing_if = "Option::is_none")]
    pub r#default: Option<String>,
}
