#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadGoalSetResponse {
    #[serde(rename = "goal")]
    pub goal: Box<crate::app_server::protocol::generated::v2::thread_goal::ThreadGoal>,
}
