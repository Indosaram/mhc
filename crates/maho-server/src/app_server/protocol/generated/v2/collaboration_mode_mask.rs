#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CollaborationModeMask {
    #[serde(rename = "name")]
    pub name: String,
    #[serde(rename = "mode", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub mode: Option<Box<crate::app_server::protocol::generated::mode_kind::ModeKind>>,
    #[serde(rename = "model", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model: Option<String>,
    #[serde(rename = "reasoning_effort", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub reasoning_effort: Option<Box<crate::app_server::protocol::generated::reasoning_effort::ReasoningEffort>>,
}
