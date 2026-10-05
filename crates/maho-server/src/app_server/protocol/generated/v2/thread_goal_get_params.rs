#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadGoalGetParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
