#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadApproveGuardianDeniedActionParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "event")]
    pub event: Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>,
}
