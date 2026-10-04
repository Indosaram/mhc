use crate::types::{Goal,GoalStatus};
const MAX_LISTED_TASKS:usize=5;
pub fn branch_todos(entries:&[maho_ext_api::SessionEntry])->Vec<maho_ext_todotools::todo_types::TodoItem> {
    let values=entries.iter().map(|entry| {
        let mut value=entry.data.clone();
        value["type"]=entry.kind.clone().into();
        value
    }).collect::<Vec<_>>();
    maho_ext_todotools::todo_storage::get_latest_todos_from_branch_entries(&values)
}
pub fn open_todo_task_contents(entries:&[maho_ext_api::SessionEntry])->Vec<String> {
    branch_todos(entries).into_iter().filter(maho_ext_todotools::todo_format::is_incomplete_todo).map(|task|task.content).collect()
}
pub fn todo_result_adds_open_tasks(details:Option<&serde_json::Value>)->bool {
    let Some(details)=details else { return false; };
    if !matches!(details["op"].as_str(),Some("init"|"append")) { return false; }
    let Some(phases)=details.get("phases").filter(|phases|maho_ext_todotools::todo_storage::is_todo_phase_array(phases)) else { return false; };
    maho_ext_todotools::todo_storage::read_todo_payload(&serde_json::json!({"phases":phases})).is_some_and(|phases|phases.iter().flat_map(|phase|&phase.tasks).any(maho_ext_todotools::todo_format::is_incomplete_todo))
}
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
    #[test] fn latest_native_branch_state_gates_completion_and_terminal_tasks_do_not() {
        let entry=|tasks|maho_ext_api::SessionEntry { id:"e".into(),parent_id:None,timestamp:"0".into(),kind:"custom".into(),data:serde_json::json!({"customType":"senpi.todo-state","data":{"schema":"v2","phases":[{"name":"Work","tasks":tasks}]}}) };
        let open=entry(serde_json::json!([{"content":"pending","status":"pending"},{"content":"running","status":"in_progress"},{"content":"done","status":"completed"},{"content":"dropped","status":"abandoned"}]));
        assert_eq!(open_todo_task_contents(&[open.clone()]),["pending","running"]);
        assert!(open_todo_task_contents(&[open,entry(serde_json::json!([]))]).is_empty());
    }
    #[test] fn only_valid_add_operations_emit_goal_reminder() {
        let mut details=serde_json::json!({"op":"append","phases":[{"name":"Work","tasks":[{"content":"work","status":"pending"}]}]});
        assert!(todo_result_adds_open_tasks(Some(&details)));
        details["op"]="done".into(); assert!(!todo_result_adds_open_tasks(Some(&details)));
        details["op"]="init".into(); details["phases"][0]["tasks"][0]["status"]="completed".into(); assert!(!todo_result_adds_open_tasks(Some(&details)));
        assert!(!todo_result_adds_open_tasks(None));
    }
    fn goal(status:GoalStatus)->Goal { serde_json::from_value(serde_json::json!({"id":"g","threadId":"s","objective":"work","status":status,"tokensUsed":0,"timeUsedSeconds":0,"createdAt":0,"updatedAt":0})).unwrap() }
    #[test] fn unfinished_goals_do_not_emit_stale_reminders() {
        for status in [GoalStatus::Active,GoalStatus::Paused,GoalStatus::Blocked] { assert!(stale_goal_todo_reminder(Some(&goal(status))).is_none()); }
    }
    #[test] fn absent_goal_emits_machine_consumed_reminder() { let reminder=stale_goal_todo_reminder(None).unwrap(); assert!(reminder.starts_with("<system-reminder>\n")); assert!(reminder.ends_with("\n</system-reminder>")); }
    #[test] fn completed_goal_emits_machine_consumed_reminder() { assert!(stale_goal_todo_reminder(Some(&goal(GoalStatus::Complete))).is_some()); }
}
