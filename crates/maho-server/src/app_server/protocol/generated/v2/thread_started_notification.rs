#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadStartedNotification {
    #[serde(rename = "thread")]
    pub thread: Box<crate::app_server::protocol::generated::v2::thread::Thread>,
}
