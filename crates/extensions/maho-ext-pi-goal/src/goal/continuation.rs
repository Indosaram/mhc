use super::types::{Goal,GoalStatus};
pub fn should_queue_goal_continuation_when_idle(goal:Option<&Goal>,is_idle:bool,has_pending_messages:bool)->bool{goal.is_some_and(|goal|goal.status==GoalStatus::Active)&&is_idle&&!has_pending_messages}
pub fn should_queue_goal_continuation_after_agent_end(goal:Option<&Goal>,has_pending_messages:bool)->bool{goal.is_some_and(|goal|goal.status==GoalStatus::Active)&&!has_pending_messages}
