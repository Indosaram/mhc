#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolRequestUserInputParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "questions")]
    pub questions: Vec<Box<crate::app_server::protocol::generated::v2::tool_request_user_input_question::ToolRequestUserInputQuestion>>,
    #[serde(rename = "autoResolutionMs", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub auto_resolution_ms: Option<f64>,
}
