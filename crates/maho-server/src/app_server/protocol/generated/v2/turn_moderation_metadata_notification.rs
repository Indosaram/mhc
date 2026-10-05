#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnModerationMetadataNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "metadata")]
    pub metadata: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
}
