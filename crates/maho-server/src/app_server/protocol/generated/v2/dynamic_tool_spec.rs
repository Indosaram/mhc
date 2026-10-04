#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DynamicToolSpecVariant01Type2 {
    #[serde(rename = "function")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DynamicToolSpecVariant01 {
    #[serde(rename = "type")]
    pub r#type: DynamicToolSpecVariant01Type2,
    #[serde(flatten)]
    pub details: Box<crate::app_server::protocol::generated::v2::dynamic_tool_function_spec::DynamicToolFunctionSpec>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DynamicToolSpecVariant13Type4 {
    #[serde(rename = "namespace")]
    Value,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DynamicToolSpecVariant13 {
    #[serde(rename = "type")]
    pub r#type: DynamicToolSpecVariant13Type4,
    #[serde(flatten)]
    pub details: Box<crate::app_server::protocol::generated::v2::dynamic_tool_namespace_spec::DynamicToolNamespaceSpec>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum DynamicToolSpec {
    Variant0(Box<DynamicToolSpecVariant01>),
    Variant1(Box<DynamicToolSpecVariant13>),
}
