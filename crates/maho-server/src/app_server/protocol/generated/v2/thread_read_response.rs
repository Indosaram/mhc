#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadReadResponse {
    #[serde(rename = "thread")]
    pub thread: Box<crate::app_server::protocol::generated::v2::thread::Thread>,
}
