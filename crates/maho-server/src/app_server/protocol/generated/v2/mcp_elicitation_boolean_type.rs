#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpElicitationBooleanType {
    #[serde(rename = "boolean")]
    Value,
}
