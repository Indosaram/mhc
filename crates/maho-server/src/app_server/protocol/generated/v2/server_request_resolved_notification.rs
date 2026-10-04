#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerRequestResolvedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "requestId")]
    pub request_id: Box<crate::app_server::protocol::generated::request_id::RequestId>,
}
