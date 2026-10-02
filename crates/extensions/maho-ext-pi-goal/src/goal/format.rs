use super::types::*;
pub fn format_goal_elapsed_seconds(value:f64)->String{
    if value.is_nan(){return "NaNh NaNm".into();}
    if value==f64::INFINITY{return "Infinityd NaNh NaNm".into();}
    let seconds=value.trunc().max(0.0);if seconds<60.0{return format!("{seconds:.0}s");}
    let minutes=(seconds/60.0).trunc();if minutes<60.0{return format!("{minutes:.0}m");}
    let hours=(minutes/60.0).trunc();let remaining_minutes=minutes%60.0;
    if hours>=24.0{return format!("{:.0}d {:.0}h {remaining_minutes:.0}m",(hours/24.0).trunc(),hours%24.0);}
    if remaining_minutes==0.0{format!("{hours:.0}h")}else{format!("{hours:.0}h {remaining_minutes:.0}m")}
}
pub fn format_tokens_compact(value:f64)->String{
    if value.is_nan(){return "NaN".into();}
    if value.is_infinite(){return if value.is_sign_negative(){"-InfinityM"}else{"InfinityM"}.into();}
    if value.trunc()==0.0{return "0".into();}
    let abs=value.abs();if abs>=1_000_000.0{format!("{}M",one_decimal(value/1_000_000.0))}else if abs>=1000.0{format!("{}K",one_decimal(value/1000.0))}else{format!("{:.0}",value.trunc())}
}
fn one_decimal(value:f64)->String{
    // At one decimal place the only exactly representable binary ties
    // have fractional part .25 or .75. Do not multiply other inputs
    // before checking: that would erase the binary error in e.g. 1.15.
    let fraction=value.abs().fract();
    let value=if fraction==0.25||fraction==0.75{(value*10.0).round()/10.0}else{value};
    let rounded=format!("{value:.1}");rounded.strip_suffix(".0").unwrap_or(&rounded).into()
}
pub const fn goal_status_label(status:GoalStatus)->&'static str{match status{GoalStatus::Active=>"active",GoalStatus::Paused=>"paused",GoalStatus::Blocked=>"blocked",GoalStatus::Complete=>"complete"}}
fn number(value:u64)->f64{value.to_string().parse().unwrap_or_else(|_|unreachable!("u64 decimal fits f64"))}
pub fn format_goal_for_tool(goal:Option<&Goal>)->Result<String,String>{
    let Some(goal)=goal else{return Ok("No active goal is set.".into());};
    let mut lines=vec![format!("Objective: {}",goal.objective),format!("Status: {}",goal_status_label(goal.status)),format!("Time used: {}",format_goal_elapsed_seconds(number(goal.time_used_seconds))),format!("Tokens used: {}",format_tokens_compact(number(goal.tokens_used)))];
    if let Some(reason)=&goal.blocked_reason&&!reason.is_empty(){lines.push(format!("Blocked reason: {reason}"));}
    if let Some(completed)=goal.completed_at.filter(|value|*value!=0){let time=chrono::DateTime::from_timestamp(i64::try_from(completed).map_err(|e|e.to_string())?,0).ok_or("Invalid time value")?;lines.push(format!("Completed at: {}",time.to_rfc3339_opts(chrono::SecondsFormat::Millis,true)));}
    Ok(lines.join("\n"))
}
pub fn goal_tool_response(goal:Option<&Goal>)->GoalToolResponse{GoalToolResponse{goal:goal.map(|goal|GoalToolSnapshot{thread_id:goal.thread_id.clone(),objective:goal.objective.clone(),status:goal.status,tokens_used:goal.tokens_used,time_used_seconds:goal.time_used_seconds,created_at:goal.created_at,updated_at:goal.updated_at,blocked_reason:goal.blocked_reason.clone(),blocked_at:goal.blocked_at})}}
pub fn format_goal_tool_response(goal:Option<&Goal>,notice:Option<&str>)->Result<String,serde_json::Error>{let response=serde_json::to_string_pretty(&goal_tool_response(goal))?;Ok(notice.map_or_else(||response.clone(),|notice|format!("{response}\n{notice}")))}
