#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct NewThreadModelDefaults {
    #[serde(rename = "model", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model: Option<String>,
    #[serde(rename = "modelReasoningEffort", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub model_reasoning_effort: Option<Box<crate::app_server::protocol::generated::reasoning_effort::ReasoningEffort>>,
    #[serde(rename = "serviceTier", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub service_tier: Option<String>,
}
