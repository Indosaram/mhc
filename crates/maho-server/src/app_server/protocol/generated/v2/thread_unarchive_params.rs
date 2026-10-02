#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadUnarchiveParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
