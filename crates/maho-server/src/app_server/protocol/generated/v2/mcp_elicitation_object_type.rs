#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpElicitationObjectType {
    #[serde(rename = "object")]
    Value,
}
