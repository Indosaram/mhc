#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Settings {
    #[serde(rename = "model")]
    pub model: String,
    #[serde(rename = "reasoning_effort", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub reasoning_effort: Option<Box<crate::app_server::protocol::generated::reasoning_effort::ReasoningEffort>>,
    #[serde(rename = "developer_instructions", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub developer_instructions: Option<String>,
}
