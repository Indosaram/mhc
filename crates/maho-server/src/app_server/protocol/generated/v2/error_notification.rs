#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ErrorNotification {
    #[serde(rename = "error")]
    pub error: Box<crate::app_server::protocol::generated::v2::turn_error::TurnError>,
    #[serde(rename = "willRetry")]
    pub will_retry: bool,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
}
