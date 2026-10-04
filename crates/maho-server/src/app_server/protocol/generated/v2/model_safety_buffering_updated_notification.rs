#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelSafetyBufferingUpdatedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "model")]
    pub model: String,
    #[serde(rename = "useCases")]
    pub use_cases: Vec<String>,
    #[serde(rename = "reasons")]
    pub reasons: Vec<String>,
    #[serde(rename = "showBufferingUi")]
    pub show_buffering_ui: bool,
    #[serde(rename = "fasterModel", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub faster_model: Option<String>,
}
