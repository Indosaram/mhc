#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolRequestUserInputOption {
    #[serde(rename = "label")]
    pub label: String,
    #[serde(rename = "description")]
    pub description: String,
}
