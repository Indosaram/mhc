#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeSdpNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "sdp")]
    pub sdp: String,
}
