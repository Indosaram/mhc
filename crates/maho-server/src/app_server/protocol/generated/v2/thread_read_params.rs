#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadReadParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "includeTurns", default, skip_serializing_if = "Option::is_none")]
    pub include_turns: Option<bool>,
}
