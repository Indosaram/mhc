use crate::{format::GoalToolRenderDetails,types::{GoalStatus,GoalToolSnapshot}};
use serde_json::Value;
pub fn status_glyph(status:GoalStatus)->&'static str { match status { GoalStatus::Active=>"●",GoalStatus::Paused=>"◌",GoalStatus::Blocked=>"■",GoalStatus::Complete=>"✓" } }
pub fn status_color(status:GoalStatus)->&'static str { match status { GoalStatus::Active=>"accent",GoalStatus::Paused=>"muted",GoalStatus::Blocked=>"error",GoalStatus::Complete=>"success" } }
pub fn shorten(value:&str,max:usize)->String {
    let units=value.encode_utf16().collect::<Vec<_>>();
    if units.len()<=max { return value.into(); }
    format!("{}…",String::from_utf16_lossy(&units[..max.saturating_sub(1)]))
}
pub fn objective_call_preview(objective:&str)->String {
    let lines=objective.split('\n').map(|line|line.trim_matches(crate::validation::js_whitespace)).filter(|line|!line.is_empty()).collect::<Vec<_>>();
    let first=lines.first().copied().unwrap_or("");
    if first.is_empty() { return String::new(); }
    let truncated=shorten(first,80);
    if lines.len()>1 && truncated==first { format!("{first}…") } else { truncated }
}
pub fn parse_render_details(value:&Value)->Option<GoalToolRenderDetails> {
    let object=value.as_object()?;
    let goal=object.get("goal")?;
    let notice=object.get("notice").and_then(Value::as_str).map(str::to_owned);
    if goal.is_null() { return Some(GoalToolRenderDetails { goal:None,notice }); }
    let status:GoalStatus=serde_json::from_value(goal.get("status")?.clone()).ok()?;
    Some(GoalToolRenderDetails { goal:Some(GoalToolSnapshot {
        thread_id:goal.get("threadId").and_then(Value::as_str).unwrap_or("").into(),
        objective:goal.get("objective")?.as_str()?.into(),status,
        tokens_used:goal.get("tokensUsed")?.as_f64()?,time_used_seconds:goal.get("timeUsedSeconds")?.as_f64()?,
        created_at:goal.get("createdAt")?.as_f64()?,updated_at:goal.get("updatedAt")?.as_f64()?,
        blocked_reason:goal.get("blockedReason").and_then(Value::as_str).map(str::to_owned),
        blocked_at:goal.get("blockedAt").and_then(Value::as_f64),
    }),notice })
}
pub fn resolve_render_details(details:Option<&Value>,text:&str)->Option<GoalToolRenderDetails> {
    if let Some(parsed)=details.and_then(parse_render_details) { return Some(parsed); }
    let text=text.trim_matches(crate::validation::js_whitespace);
    if !text.starts_with('{') { return None; }
    parse_render_details(&serde_json::from_str::<Value>(text).ok()?)
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn direct_null_goal_details_override_response_text() {
        let details=serde_json::json!({"goal":null,"notice":"hint"});
        let result=resolve_render_details(Some(&details),"invalid").unwrap();
        assert!(result.goal.is_none()); assert_eq!(result.notice.as_deref(),Some("hint"));
    }
    #[test] fn invalid_direct_details_fall_back_to_machine_response() {
        let result=resolve_render_details(Some(&serde_json::json!({"goal":{}})),"{\"goal\":null}").unwrap();
        assert!(result.goal.is_none());
    }
    #[test] fn non_json_response_does_not_make_widget_details() { assert!(resolve_render_details(None,"plain output").is_none()); }
    #[test] fn snapshot_accepts_numeric_fractional_and_negative_fields_like_upstream() {
        let result=parse_render_details(&serde_json::json!({"goal":{"objective":"work","status":"active","tokensUsed":1.5,"timeUsedSeconds":-2.5,"createdAt":-1.0,"updatedAt":2.5}})).unwrap().goal.unwrap();
        assert_eq!(result.tokens_used,1.5); assert_eq!(result.created_at,-1.0); assert_eq!(result.updated_at,2.5);
    }
}
