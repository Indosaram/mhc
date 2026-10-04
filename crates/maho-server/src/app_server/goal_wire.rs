use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus { Active, Paused, Blocked, Complete }
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGoal {
    pub thread_id: String,
    pub objective: String,
    pub status: GoalStatus,
    pub token_budget: Option<f64>,
    pub tokens_used: f64,
    pub time_used_seconds: f64,
    pub created_at: f64,
    pub updated_at: f64,
}
