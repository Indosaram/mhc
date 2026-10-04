#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadCompactStartParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
