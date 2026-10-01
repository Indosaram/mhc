use crate::types::Goal;
pub const GOAL_ELAPSED_TICK_INTERVAL_MS:u64=1000;
pub fn goal_live_elapsed_seconds(goal:&Goal,measured_from_milliseconds:f64,now_milliseconds:f64)->f64 {
    goal.time_used_seconds+((now_milliseconds-measured_from_milliseconds)/1000.0+0.5).floor().max(0.0)
}
#[cfg(test)] mod tests {
    use super::*;
    fn goal()->Goal { serde_json::from_value(serde_json::json!({"id":"g","threadId":"s","objective":"work","status":"active","tokensUsed":0,"timeUsedSeconds":10.0,"createdAt":0,"updatedAt":0})).unwrap() }
    #[test] fn live_elapsed_rounds_positive_half_seconds() { let goal=goal(); let result=[goal_live_elapsed_seconds(&goal,1000.0,1499.0),goal_live_elapsed_seconds(&goal,1000.0,1500.0)]; assert_eq!(result,[10.0,11.0]); }
    #[test] fn backward_clock_never_subtracts_committed_elapsed() { let result=goal_live_elapsed_seconds(&goal(),1000.0,0.0); assert_eq!(result,10.0); }
}
