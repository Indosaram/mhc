#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRealtimeTranscriptDoneNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "role")]
    pub role: String,
    #[serde(rename = "text")]
    pub text: String,
}
