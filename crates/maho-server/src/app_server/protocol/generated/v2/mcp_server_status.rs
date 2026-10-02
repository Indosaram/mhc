pub type McpServerStatusTools1 = std::collections::BTreeMap<String, Box<crate::app_server::protocol::generated::tool::Tool>>;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpServerStatus {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "serverInfo", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub server_info: Option<Box<crate::app_server::protocol::generated::mcp_server_info::McpServerInfo>>,
    #[serde(rename = "tools")]
    pub tools: McpServerStatusTools1,
    #[serde(rename = "resources")]
    pub resources: Vec<Box<crate::app_server::protocol::generated::resource::Resource>>,
    #[serde(rename = "resourceTemplates")]
    pub resource_templates: Vec<Box<crate::app_server::protocol::generated::resource_template::ResourceTemplate>>,
    #[serde(rename = "authStatus")]
    pub auth_status: Box<crate::app_server::protocol::generated::v2::mcp_auth_status::McpAuthStatus>,
}
