use crate::types::{Goal,GoalStatus};
const MAX_LISTED_TASKS:usize=5;
pub fn open_todo_completion_error(open_tasks:&[String])->String {
    let listed=open_tasks.iter().take(MAX_LISTED_TASKS).map(|task|format!("\"{task}\"")).collect::<Vec<_>>().join(", ");
    let suffix=if open_tasks.len()>MAX_LISTED_TASKS { format!(" and {} more",open_tasks.len()-MAX_LISTED_TASKS) } else { String::new() };
    format!("cannot mark the goal complete: {} open todo task(s) remain: {listed}{suffix}. Do the remaining work, or drop the tasks that are genuinely no longer needed - closing an unfinished task to clear this gate reports a completion that did not happen. Then run the completion audit again and retry update_goal.",open_tasks.len())
}
pub fn stale_goal_todo_reminder(goal:Option<&Goal>)->Option<String> {
    if goal.is_some_and(|goal|goal.status!=GoalStatus::Complete) { return None; }
    let (stale,fix)=if goal.is_none() {
        ("New todo tasks were added, but this thread has no goal registered.","If this todo list tracks a durable objective (multi-step work that should survive across turns), register it now with create_goal so progress is tracked and audited.")
    } else {
        ("New todo tasks were added, but the registered goal is already complete (stale), so the new work is untracked.","If this todo list tracks a durable objective (multi-step work that should survive across turns), register it now with create_goal; creating a new goal archives the completed one and replaces it.")
    };
    Some(["<system-reminder>",stale,fix,"If the todos are trivial short-lived bookkeeping for the current turn, continue without a goal.","</system-reminder>"].join("\n"))
}
#[cfg(test)] mod tests {
    use super::*;
    fn goal(status:GoalStatus)->Goal { serde_json::from_value(serde_json::json!({"id":"g","threadId":"s","objective":"work","status":status,"tokensUsed":0,"timeUsedSeconds":0,"createdAt":0,"updatedAt":0})).unwrap() }
    #[test] fn unfinished_goals_do_not_emit_stale_reminders() {
        for status in [GoalStatus::Active,GoalStatus::Paused,GoalStatus::Blocked] { assert!(stale_goal_todo_reminder(Some(&goal(status))).is_none()); }
    }
    #[test] fn absent_goal_emits_machine_consumed_reminder() { let reminder=stale_goal_todo_reminder(None).unwrap(); assert!(reminder.starts_with("<system-reminder>\n")); assert!(reminder.ends_with("\n</system-reminder>")); }
    #[test] fn completed_goal_emits_machine_consumed_reminder() { assert!(stale_goal_todo_reminder(Some(&goal(GoalStatus::Complete))).is_some()); }
}
