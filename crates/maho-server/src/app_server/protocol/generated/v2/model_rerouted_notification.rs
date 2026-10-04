#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelReroutedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "fromModel")]
    pub from_model: String,
    #[serde(rename = "toModel")]
    pub to_model: String,
    #[serde(rename = "reason")]
    pub reason: Box<crate::app_server::protocol::generated::v2::model_reroute_reason::ModelRerouteReason>,
}
