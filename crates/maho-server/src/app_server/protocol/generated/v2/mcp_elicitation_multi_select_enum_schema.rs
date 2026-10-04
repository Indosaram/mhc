#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum McpElicitationMultiSelectEnumSchema {
    Variant0(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_untitled_multi_select_enum_schema::McpElicitationUntitledMultiSelectEnumSchema>>),
    Variant1(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_titled_multi_select_enum_schema::McpElicitationTitledMultiSelectEnumSchema>>),
}
