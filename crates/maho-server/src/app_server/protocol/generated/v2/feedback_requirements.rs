#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FeedbackRequirements {
    #[serde(rename = "enabled", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub enabled: Option<bool>,
}
