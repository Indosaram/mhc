use crate::types::{Goal, GoalStatus, GoalToolResponse, GoalToolSnapshot};
pub fn format_goal_elapsed_seconds(value:f64)->String {
    let seconds=value.trunc().max(0.0); if seconds<60.0 { return format!("{seconds}s"); }
    let minutes=(seconds/60.0).trunc(); if minutes<60.0 { return format!("{minutes}m"); }
    let hours=(minutes/60.0).trunc(); let remaining=minutes%60.0;
    if hours>=24.0 { return format!("{}d {}h {remaining}m",(hours/24.0).trunc(),hours%24.0); }
    if remaining==0.0 { format!("{hours}h") } else { format!("{hours}h {remaining}m") }
}
fn one_decimal(value:f64)->String { let rendered=format!("{value:.1}"); rendered.strip_suffix(".0").unwrap_or(&rendered).into() }
pub fn format_tokens_compact(value:f64)->String { if value.abs()>=1_000_000.0 { format!("{}M",one_decimal(value/1_000_000.0)) } else if value.abs()>=1000.0 { format!("{}K",one_decimal(value/1000.0)) } else { format!("{}",value.trunc()) } }
pub const fn goal_status_label(status:GoalStatus)->&'static str { match status { GoalStatus::Active=>"active",GoalStatus::Paused=>"paused",GoalStatus::Blocked=>"blocked",GoalStatus::Complete=>"complete" } }
#[derive(Clone, Debug, PartialEq)]
pub struct GoalToolRenderDetails { pub goal:Option<GoalToolSnapshot>,pub notice:Option<String> }
pub fn goal_tool_snapshot(goal:&Goal)->GoalToolSnapshot { GoalToolSnapshot { thread_id:goal.thread_id.clone(),objective:goal.objective.clone(),status:goal.status,tokens_used:goal.tokens_used,time_used_seconds:goal.time_used_seconds,created_at:goal.created_at,updated_at:goal.updated_at,blocked_reason:goal.blocked_reason.clone(),blocked_at:goal.blocked_at } }
pub fn goal_tool_render_details(goal:Option<&Goal>,notice:Option<&str>)->GoalToolRenderDetails { GoalToolRenderDetails { goal:goal.map(goal_tool_snapshot),notice:notice.map(str::to_owned) } }
pub fn goal_tool_response(goal:Option<&Goal>)->GoalToolResponse { GoalToolResponse { goal:goal.map(goal_tool_snapshot) } }
pub fn format_goal_tool_response(goal:Option<&Goal>,notice:Option<&str>)->Result<String,serde_json::Error> { let response=serde_json::to_string_pretty(&goal_tool_response(goal))?; Ok(notice.map_or_else(||response.clone(),|notice|format!("{response}\n{notice}"))) }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn elapsed_duration_selects_units() { let result=[0.0,59.9,60.0,3599.0,3600.0,3660.0,90060.0].map(format_goal_elapsed_seconds); assert_eq!(result,["0s","59s","1m","59m","1h","1h 1m","1d 1h 1m"]); }
    #[test] fn negative_elapsed_is_zero() { let result=format_goal_elapsed_seconds(-20.0); assert_eq!(result,"0s"); }
    #[test] fn compact_tokens_select_magnitudes() { let result=[999.9,1000.0,1200.0,1000000.0,1500000.0].map(format_tokens_compact); assert_eq!(result,["999","1K","1.2K","1M","1.5M"]); }
    #[test] fn absent_goal_response_is_machine_readable() { let result=format_goal_tool_response(None,None).unwrap(); let parsed:serde_json::Value=serde_json::from_str(&result).unwrap(); assert_eq!(parsed,serde_json::json!({"goal":null})); }
}
