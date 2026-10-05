#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum McpElicitationEnumSchema {
    Variant0(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_single_select_enum_schema::McpElicitationSingleSelectEnumSchema>>),
    Variant1(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_multi_select_enum_schema::McpElicitationMultiSelectEnumSchema>>),
    Variant2(Box<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_legacy_titled_enum_schema::McpElicitationLegacyTitledEnumSchema>>),
}
