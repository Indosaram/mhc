#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FuzzyFileSearchSessionCompletedNotification {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}
