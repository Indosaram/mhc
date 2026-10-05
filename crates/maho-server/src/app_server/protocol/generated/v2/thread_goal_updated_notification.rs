#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadGoalUpdatedNotification {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "turnId", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub turn_id: Option<String>,
    #[serde(rename = "goal")]
    pub goal: Box<crate::app_server::protocol::generated::v2::thread_goal::ThreadGoal>,
}
