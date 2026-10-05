#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum McpElicitationPrimitiveSchema {
    Variant0(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_enum_schema::McpElicitationEnumSchema>>),
    Variant1(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_string_schema::McpElicitationStringSchema>>),
    Variant2(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_number_schema::McpElicitationNumberSchema>>),
    Variant3(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_boolean_schema::McpElicitationBooleanSchema>>),
}
