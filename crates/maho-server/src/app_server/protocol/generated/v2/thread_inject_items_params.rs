#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadInjectItemsParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "items")]
    pub items: Vec<Box<crate::app_server::protocol::generated::serde_json::json_value::JsonValue>>,
}
