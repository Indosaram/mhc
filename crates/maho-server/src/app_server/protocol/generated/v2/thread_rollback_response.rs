#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadRollbackResponse {
    #[serde(rename = "thread")]
    pub thread: Box<crate::app_server::protocol::generated::v2::thread::Thread>,
}
