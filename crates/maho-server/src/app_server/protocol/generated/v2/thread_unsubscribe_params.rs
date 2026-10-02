#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadUnsubscribeParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
