#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct McpElicitationConstOption {
    #[serde(rename = "const")]
    pub r#const: String,
    #[serde(rename = "title")]
    pub title: String,
}
