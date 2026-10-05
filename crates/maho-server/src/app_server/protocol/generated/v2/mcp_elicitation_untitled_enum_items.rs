#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpElicitationUntitledEnumItems {
    #[serde(rename = "type")]
    pub r#type: Box<crate::app_server::protocol::generated::v2::mcp_elicitation_string_type::McpElicitationStringType>,
    #[serde(rename = "enum")]
    pub r#enum: Vec<String>,
}
