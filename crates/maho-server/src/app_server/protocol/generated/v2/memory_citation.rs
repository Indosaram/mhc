#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryCitation {
    #[serde(rename = "entries")]
    pub entries: Vec<Box<crate::app_server::protocol::generated::v2::memory_citation_entry::MemoryCitationEntry>>,
    #[serde(rename = "threadIds")]
    pub thread_ids: Vec<String>,
}
