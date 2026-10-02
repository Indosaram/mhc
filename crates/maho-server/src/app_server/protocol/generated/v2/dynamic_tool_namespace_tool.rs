#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DynamicToolNamespaceToolType1 {
    #[serde(rename = "function")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DynamicToolNamespaceTool {
    #[serde(rename = "type")]
    pub r#type: DynamicToolNamespaceToolType1,
    #[serde(flatten)]
    pub details: Box<crate::app_server::protocol::generated::v2::dynamic_tool_function_spec::DynamicToolFunctionSpec>,
}
