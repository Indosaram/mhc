#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadSetNameParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "name")]
    pub name: String,
}
