use crate::types::{Goal, GoalStatus, GoalToolResponse, GoalToolSnapshot};
pub fn format_goal_elapsed_seconds(value:f64)->String {
    let number=maho_ai::utils::js::number_to_string;
    let seconds=if value.is_nan() { value } else { value.trunc().max(0.0) }; if seconds<60.0 { return format!("{}s",number(seconds)); }
    let minutes=(seconds/60.0).trunc(); if minutes<60.0 { return format!("{}m",number(minutes)); }
    let hours=(minutes/60.0).trunc(); let remaining=minutes%60.0;
    if hours>=24.0 { return format!("{}d {}h {}m",number((hours/24.0).trunc()),number(hours%24.0),number(remaining)); }
    if remaining==0.0 { format!("{}h",number(hours)) } else { format!("{}h {}m",number(hours),number(remaining)) }
}
pub(crate) fn fixed_decimal(value:f64,places:u32)->String {
    if !value.is_finite() || value.abs()>=1e21 { return maho_ai::utils::js::number_to_string(value); }
    let bits=value.abs().to_bits(); let exponent=((bits>>52)&0x7ff) as i32;
    let mantissa=u128::from(bits&((1_u64<<52)-1))|if exponent==0 { 0 } else { 1_u128<<52 };
    let shift=if exponent==0 { -1074 } else { exponent-1023-52 };
    let scale=10_u128.pow(places); let scaled=mantissa*scale;
    let rounded=if shift>=0 { scaled<<shift } else {
        let right=(-shift) as u32;
        if right>=128 { 0 } else { (scaled>>right)+u128::from((scaled&((1_u128<<right)-1))>=(1_u128<<(right-1))) }
    };
    let sign=if value<0.0 { "-" } else { "" };
    format!("{sign}{}.{:0width$}",rounded/scale,rounded%scale,width=places as usize)
}
fn one_decimal(value:f64)->String { let rendered=fixed_decimal(value,1); rendered.strip_suffix(".0").unwrap_or(&rendered).into() }
pub fn format_tokens_compact(value:f64)->String { if value.abs()>=1_000_000.0 { format!("{}M",one_decimal(value/1_000_000.0)) } else if value.abs()>=1000.0 { format!("{}K",one_decimal(value/1000.0)) } else { maho_ai::utils::js::number_to_string(value.trunc()) } }
pub const fn goal_status_label(status:GoalStatus)->&'static str { match status { GoalStatus::Active=>"active",GoalStatus::Paused=>"paused",GoalStatus::Blocked=>"blocked",GoalStatus::Complete=>"complete" } }
pub fn iso_timestamp(seconds:u64)->Option<String> {
    if seconds>8_640_000_000_000 { return None; }
    let days=i64::try_from(seconds/86400).ok()?+719468;
    let era=days/146097; let day=days-era*146097;
    let year_of_era=(day-day/1460+day/36524-day/146096)/365;
    let day_of_year=day-(365*year_of_era+year_of_era/4-year_of_era/100);
    let month_prime=(5*day_of_year+2)/153; let date=day_of_year-(153*month_prime+2)/5+1;
    let month=month_prime+if month_prime<10 { 3 } else { -9 };
    let year=year_of_era+era*400+i64::from(month<=2);
    let year=if year<=9999 { format!("{year:04}") } else { format!("+{year:06}") };
    Some(format!("{year}-{month:02}-{date:02}T{:02}:{:02}:{:02}.000Z",seconds/3600%24,seconds/60%60,seconds%60))
}
pub fn format_goal_for_tool(goal:Option<&Goal>)->Result<String,crate::errors::GoalError> {
    let Some(goal)=goal else { return Ok("No active goal is set.".into()); };
    let mut lines=vec![format!("Objective: {}",goal.objective),format!("Status: {}",goal_status_label(goal.status)),format!("Time used: {}",format_goal_elapsed_seconds(goal.time_used_seconds)),format!("Tokens used: {}",format_tokens_compact(goal.tokens_used as f64))];
    if let Some(reason)=&goal.blocked_reason && !reason.is_empty() { lines.push(format!("Blocked reason: {reason}")); }
    if let Some(at)=goal.completed_at && at!=0 { let timestamp=iso_timestamp(at).ok_or_else(||crate::errors::GoalError::InvalidMutation("Invalid time value".into()))?; lines.push(format!("Completed at: {timestamp}")); }
    Ok(lines.join("\n"))
}
#[derive(Clone, Debug, PartialEq)]
pub struct GoalToolRenderDetails { pub goal:Option<GoalToolSnapshot>,pub notice:Option<String> }
pub fn goal_tool_snapshot(goal:&Goal)->GoalToolSnapshot { GoalToolSnapshot { thread_id:goal.thread_id.clone(),objective:goal.objective.clone(),status:goal.status,tokens_used:goal.tokens_used as f64,time_used_seconds:goal.time_used_seconds,created_at:goal.created_at as f64,updated_at:goal.updated_at as f64,blocked_reason:goal.blocked_reason.clone(),blocked_at:goal.blocked_at.map(|value|value as f64) } }
pub fn goal_tool_render_details(goal:Option<&Goal>,notice:Option<&str>)->GoalToolRenderDetails { GoalToolRenderDetails { goal:goal.map(goal_tool_snapshot),notice:notice.map(str::to_owned) } }
pub fn goal_tool_response(goal:Option<&Goal>)->GoalToolResponse { GoalToolResponse { goal:goal.map(goal_tool_snapshot) } }
pub fn format_goal_tool_response(goal:Option<&Goal>,notice:Option<&str>)->Result<String,serde_json::Error> { let response=maho_ai::utils::js::json_stringify_pretty(&serde_json::to_value(goal_tool_response(goal))?); Ok(notice.map_or_else(||response.clone(),|notice|format!("{response}\n{notice}"))) }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn elapsed_duration_selects_units() { let result=[0.0,59.9,60.0,3599.0,3600.0,3660.0,90060.0].map(format_goal_elapsed_seconds); assert_eq!(result,["0s","59s","1m","59m","1h","1h 1m","1d 1h 1m"]); }
    #[test] fn negative_elapsed_is_zero() { let result=format_goal_elapsed_seconds(-20.0); assert_eq!(result,"0s"); }
    #[test] fn nonfinite_and_signed_zero_labels_match_javascript() {
        assert_eq!(format_tokens_compact(-0.0),"0"); assert_eq!(format_tokens_compact(f64::NAN),"NaN"); assert_eq!(format_goal_elapsed_seconds(f64::NAN),"NaNh NaNm"); assert_eq!(format_goal_elapsed_seconds(f64::INFINITY),"Infinityd NaNh NaNm");
    }
    #[test] fn compact_tokens_select_magnitudes() { let result=[999.9,1000.0,1200.0,1000000.0,1500000.0].map(format_tokens_compact); assert_eq!(result,["999","1K","1.2K","1M","1.5M"]); }
    #[test] fn absent_goal_response_is_machine_readable() { let result=format_goal_tool_response(None,None).unwrap(); let parsed:serde_json::Value=serde_json::from_str(&result).unwrap(); assert_eq!(parsed,serde_json::json!({"goal":null})); }
    #[test] fn one_decimal_uses_exact_binary_value_and_ties_away_from_zero() {
        for (value,expected) in [(1.25,"1.3"),(-1.25,"-1.3"),(1.15,"1.1"),(2.55,"2.5"),(0.0,"0"),(-0.0,"0"),(1e21,"1e+21")] { assert_eq!(one_decimal(value),expected,"{value}"); }
    }
    #[test] fn iso_dates_cover_javascript_extended_years_and_timeclip() {
        assert_eq!(iso_timestamp(0).as_deref(),Some("1970-01-01T00:00:00.000Z"));
        assert_eq!(iso_timestamp(253402300800).as_deref(),Some("+010000-01-01T00:00:00.000Z"));
        assert_eq!(iso_timestamp(8_640_000_000_000).as_deref(),Some("+275760-09-13T00:00:00.000Z"));
        assert!(iso_timestamp(8_640_000_000_001).is_none());
    }
}
