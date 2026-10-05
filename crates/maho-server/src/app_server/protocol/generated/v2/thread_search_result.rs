#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadSearchResult {
    #[serde(rename = "thread")]
    pub thread: Box<crate::app_server::protocol::generated::v2::thread::Thread>,
    #[serde(rename = "snippet")]
    pub snippet: String,
}
