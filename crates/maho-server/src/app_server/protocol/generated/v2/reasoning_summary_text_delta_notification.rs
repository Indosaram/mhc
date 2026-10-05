#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReasoningSummaryTextDeltaNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "itemId")]
    pub item_id: String,
    #[serde(rename = "delta")]
    pub delta: String,
    #[serde(rename = "summaryIndex")]
    pub summary_index: f64,
}
