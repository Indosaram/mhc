#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpElicitationUntitledMultiSelectEnumSchema {
    #[serde(rename = "type")]
    pub r#type: Box<crate::app_server::protocol::generated::v2::mcp_elicitation_array_type::McpElicitationArrayType>,
    #[serde(rename = "title", default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(rename = "description", default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "minItems", default, skip_serializing_if = "Option::is_none")]
    pub min_items: Option<i64>,
    #[serde(rename = "maxItems", default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<i64>,
    #[serde(rename = "items")]
    pub items: Box<crate::app_server::protocol::generated::v2::mcp_elicitation_untitled_enum_items::McpElicitationUntitledEnumItems>,
    #[serde(rename = "default", default, skip_serializing_if = "Option::is_none")]
    pub r#default: Option<Vec<String>>,
}
