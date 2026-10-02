#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadShellCommandParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "command")]
    pub command: String,
}
