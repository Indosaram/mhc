#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadArchiveParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
