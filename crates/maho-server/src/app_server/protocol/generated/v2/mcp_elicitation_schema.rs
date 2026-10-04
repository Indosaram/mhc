pub type McpElicitationSchemaProperties1 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::v2::mcp_elicitation_primitive_schema::McpElicitationPrimitiveSchema>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpElicitationSchema {
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub _schema: Option<String>,
    #[serde(rename = "type")]
    pub r#type: Box<crate::app_server::protocol::generated::v2::mcp_elicitation_object_type::McpElicitationObjectType>,
    #[serde(rename = "properties")]
    pub properties: McpElicitationSchemaProperties1,
    #[serde(rename = "required", default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}
