#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum McpElicitationSingleSelectEnumSchema {
    Variant0(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_untitled_single_select_enum_schema::McpElicitationUntitledSingleSelectEnumSchema>>),
    Variant1(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_titled_single_select_enum_schema::McpElicitationTitledSingleSelectEnumSchema>>),
}
