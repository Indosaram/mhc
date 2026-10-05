#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum McpElicitationNumberType {
    #[serde(rename = "number")]
    Number,
    #[serde(rename = "integer")]
    Integer,
}
