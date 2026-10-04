#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpElicitationTitledEnumItems {
    #[serde(rename = "anyOf")]
    pub any_of: Vec<Box<crate::app_server::protocol::generated::v2::mcp_elicitation_const_option::McpElicitationConstOption>>,
}
