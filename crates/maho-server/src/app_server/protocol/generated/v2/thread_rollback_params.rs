#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRollbackParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "numTurns")]
    pub num_turns: f64,
}
