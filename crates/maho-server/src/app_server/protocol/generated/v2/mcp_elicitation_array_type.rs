#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpElicitationArrayType {
    #[serde(rename = "array")]
    Value,
}
