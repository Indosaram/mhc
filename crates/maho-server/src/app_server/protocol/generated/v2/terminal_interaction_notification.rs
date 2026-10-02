#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TerminalInteractionNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "processId")]
    pub process_id: String,
    #[serde(rename = "stdin")]
    pub stdin: String,
}
