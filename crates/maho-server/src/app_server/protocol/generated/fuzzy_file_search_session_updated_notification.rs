#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FuzzyFileSearchSessionUpdatedNotification {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "query")]
    pub query: String,
    #[serde(rename = "files")]
    pub files: Vec<Box<crate::app_server::protocol::generated::fuzzy_file_search_result::FuzzyFileSearchResult>>,
}
