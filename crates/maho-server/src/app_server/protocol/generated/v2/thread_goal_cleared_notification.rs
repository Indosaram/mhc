#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadGoalClearedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
