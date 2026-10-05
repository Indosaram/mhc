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

pub fn to_thread_goal(goal: &maho_ext_goal::Goal) -> ThreadGoal {
    ThreadGoal {
        thread_id: goal.thread_id.clone(),
        objective: goal.objective.clone(),
        status: match goal.status {
            maho_ext_goal::GoalStatus::Active => GoalStatus::Active,
            maho_ext_goal::GoalStatus::Paused => GoalStatus::Paused,
            maho_ext_goal::GoalStatus::Blocked => GoalStatus::Blocked,
            maho_ext_goal::GoalStatus::Complete => GoalStatus::Complete,
        },
        token_budget: goal.token_budget.map(|budget| budget as f64),
        tokens_used: goal.tokens_used as f64,
        time_used_seconds: goal.time_used_seconds,
        created_at: goal.created_at as f64,
        updated_at: goal.updated_at as f64,
    }
}
