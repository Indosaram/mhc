#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpElicitationStringType {
    #[serde(rename = "string")]
    Value,
}
