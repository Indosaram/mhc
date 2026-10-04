#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadGoalGetResponse {
    #[serde(rename = "goal", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub goal: Option<Box<crate::app_server::protocol::generated::v2::thread_goal::ThreadGoal>>,
}
