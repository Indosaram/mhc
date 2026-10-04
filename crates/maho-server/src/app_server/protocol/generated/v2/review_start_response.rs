#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewStartResponse {
    #[serde(rename = "turn")]
    pub turn: Box<crate::app_server::protocol::generated::v2::turn::Turn>,
    #[serde(rename = "reviewThreadId")]
    pub review_thread_id: String,
}
