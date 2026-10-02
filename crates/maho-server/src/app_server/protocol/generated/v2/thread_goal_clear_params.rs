#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadGoalClearParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
