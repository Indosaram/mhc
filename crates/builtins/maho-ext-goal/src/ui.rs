use maho_ext_api::ExtensionContext;
use crate::{format::format_goal_elapsed_seconds,types::{Goal,GoalStatus},validation::js_whitespace};
pub const STATUS_KEY:&str="goal";
pub const OBJECTIVE_PREVIEW_MAX_LENGTH:usize=32;
pub fn truncate_goal_objective(objective:&str,max_length:usize)->String {
    let normalized=objective.split(js_whitespace).filter(|part|!part.is_empty()).collect::<Vec<_>>().join(" ");
    if normalized.encode_utf16().count()<=max_length { return normalized; }
    let truncated=String::from_utf16_lossy(&normalized.encode_utf16().take(max_length.saturating_sub(1)).collect::<Vec<_>>()); format!("{}\u{2026}",truncated.trim_end_matches(js_whitespace))
}
pub fn goal_status_text(goal:&Goal,live_elapsed_seconds:Option<f64>)->String {
    match goal.status {
        GoalStatus::Active=>if let Some(seconds)=live_elapsed_seconds { format!("Pursuing goal ({})",format_goal_elapsed_seconds(seconds)) } else if goal.time_used_seconds>0.0 { format!("Pursuing goal ({})",format_goal_elapsed_seconds(goal.time_used_seconds)) } else { "Pursuing goal".into() },
        GoalStatus::Paused=>"Goal paused (/goal resume)".into(),
        GoalStatus::Blocked=>goal.blocked_reason.as_ref().filter(|reason|!reason.is_empty()).map_or_else(||"Goal blocked".into(),|reason|format!("Goal blocked: {reason}")),
        GoalStatus::Complete=>{ let elapsed=if goal.time_used_seconds>0.0 { format!(" ({})",format_goal_elapsed_seconds(goal.time_used_seconds)) } else { String::new() }; format!("{} \u{b7} Goal achieved{elapsed}",truncate_goal_objective(&goal.objective,OBJECTIVE_PREVIEW_MAX_LENGTH)) },
    }
}
pub fn update_goal_ui(ctx:&ExtensionContext,goal:Option<&Goal>,live_elapsed_seconds:Option<f64>) { if !ctx.has_ui { return; } let status=goal.map(|goal|goal_status_text(goal,live_elapsed_seconds)); ctx.ui.set_status(STATUS_KEY,status.as_deref()); }
