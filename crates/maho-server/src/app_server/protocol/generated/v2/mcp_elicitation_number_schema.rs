#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpElicitationNumberSchema {
    #[serde(rename = "type")]
    pub r#type: Box<crate::app_server::protocol::generated::v2::mcp_elicitation_number_type::McpElicitationNumberType>,
    #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "minimum", default, skip_serializing_if = "Option::is_none")]
    pub minimum: Option<f64>,
    #[serde(rename = "maximum", default, skip_serializing_if = "Option::is_none")]
    pub maximum: Option<f64>,
    #[serde(rename = "default", default, skip_serializing_if = "Option::is_none")]
    pub r#default: Option<f64>,
}
