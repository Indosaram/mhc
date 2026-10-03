use maho_ext_api::types::ExtensionContext;
use super::{format::format_goal_elapsed_seconds,types::{Goal,GoalStatus}};
pub const STATUS_KEY:&str="goal";
pub fn update_goal_ui(context:&ExtensionContext,goal:Option<&Goal>){if !context.has_ui{return;}let text=goal.map(goal_status_text);context.ui.set_status(STATUS_KEY,text.as_deref());}
pub fn goal_status_text(goal:&Goal)->String{match goal.status{
    GoalStatus::Active=>if goal.time_used_seconds>0.0{format!("Pursuing goal ({})",format_goal_elapsed_seconds(goal.time_used_seconds))}else{"Pursuing goal".into()},
    GoalStatus::Paused=>"Goal paused (/goal resume)".into(),
    GoalStatus::Blocked=>"Goal blocked".into(),
    GoalStatus::Complete=>"Goal achieved".into(),
}}
