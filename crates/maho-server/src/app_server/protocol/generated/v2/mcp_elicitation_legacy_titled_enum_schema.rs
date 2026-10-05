#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpElicitationLegacyTitledEnumSchema {
    #[serde(rename = "type")]
    pub r#type: Box<crate::app_server::protocol::generated::v2::mcp_elicitation_string_type::McpElicitationStringType>,
    #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "enum")]
    pub r#enum: Vec<String>,
    #[serde(rename = "enumNames", default, skip_serializing_if = "Option::is_none")]
    pub enum_names: Option<Vec<String>>,
    #[serde(rename = "default", default, skip_serializing_if = "Option::is_none")]
    pub r#default: Option<String>,
}
