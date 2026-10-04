#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadUnsubscribeResponse {
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::thread_unsubscribe_status::ThreadUnsubscribeStatus>,
}
