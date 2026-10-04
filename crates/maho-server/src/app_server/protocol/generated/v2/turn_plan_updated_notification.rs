#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TurnPlanUpdatedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId")]
    pub turn_id: String,
    #[serde(rename = "explanation", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub explanation: Option<String>,
    #[serde(rename = "plan")]
    pub plan: Vec<Box<crate::app_server::protocol::generated::v2::turn_plan_step::TurnPlanStep>>,
}
