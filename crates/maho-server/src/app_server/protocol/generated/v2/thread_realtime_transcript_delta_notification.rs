#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeTranscriptDeltaNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "role")]
    pub role: String,
    #[serde(rename = "delta")]
    pub delta: String,
}
