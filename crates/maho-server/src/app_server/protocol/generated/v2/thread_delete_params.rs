#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadDeleteParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
}
