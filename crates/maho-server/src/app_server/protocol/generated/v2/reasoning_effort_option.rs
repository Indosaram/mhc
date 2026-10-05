#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReasoningEffortOption {
    #[serde(rename = "reasoningEffort")]
    pub reasoning_effort: Box<crate::app_server::protocol::generated::reasoning_effort::ReasoningEffort>,
    #[serde(rename = "description")]
    pub description: String,
}
