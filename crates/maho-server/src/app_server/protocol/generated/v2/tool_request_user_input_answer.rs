#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolRequestUserInputAnswer {
    #[serde(rename = "answers")]
    pub answers: Vec<String>,
}
