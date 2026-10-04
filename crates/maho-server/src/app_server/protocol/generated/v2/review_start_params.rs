#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ReviewStartParams {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "target")]
    pub target: Box<crate::app_server::protocol::generated::v2::review_target::ReviewTarget>,
    #[serde(rename = "delivery", default, skip_serializing_if = "Option::is_none", deserialize_with = "crate::app_server::protocol::nullable::deserialize")]
    pub delivery: Option<Option<Box<crate::app_server::protocol::generated::v2::review_delivery::ReviewDelivery>>>,
}
