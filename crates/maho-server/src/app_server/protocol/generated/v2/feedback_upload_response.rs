#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FeedbackUploadResponse {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
