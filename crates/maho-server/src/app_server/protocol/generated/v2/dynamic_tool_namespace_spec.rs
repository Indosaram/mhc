#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DynamicToolNamespaceSpec {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "description")]
    pub description: String,
    #[serde(rename = "tools")]
    pub tools: Vec<Box<crate::app_server::protocol::generated::v2::dynamic_tool_namespace_tool::DynamicToolNamespaceTool>>,
}
