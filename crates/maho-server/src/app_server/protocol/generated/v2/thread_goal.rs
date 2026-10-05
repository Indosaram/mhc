#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadGoal {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "objective")]
    pub objective: String,
    #[serde(rename = "status")]
    pub status: Box<crate::app_server::protocol::generated::v2::thread_goal_status::ThreadGoalStatus>,
    #[serde(rename = "tokenBudget", deserialize_with = "crate::app_server::protocol::nullable::deserialize_required")]
    pub token_budget: Option<f64>,
    #[serde(rename = "tokensUsed")]
    pub tokens_used: f64,
    #[serde(rename = "timeUsedSeconds")]
    pub time_used_seconds: f64,
    #[serde(rename = "createdAt")]
    pub created_at: f64,
    #[serde(rename = "updatedAt")]
    pub updated_at: f64,
}
