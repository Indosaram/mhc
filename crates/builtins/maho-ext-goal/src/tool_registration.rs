use crate::types::ModelSettableGoalStatus;
use maho_ext_api::ToolError;
use serde_json::{Value,json};
pub fn create_goal_schema()->Value { json!({"type":"object","properties":{"objective":{"type":"string","description":"Required. The concrete objective to start pursuing. Limit: 4,000 characters. For longer instructions, put the full objective in a file and refer to that file."}},"required":["objective"],"additionalProperties":false}) }
pub fn update_goal_schema()->Value { json!({"type":"object","properties":{"status":{"anyOf":[{"const":"complete","type":"string"},{"const":"blocked","type":"string"}],"description":"Required. Set to complete when achieved or blocked with a non-empty reason."},"reason":{"type":"string","description":"Required and non-empty when status is blocked; rejected when status is complete."}},"required":["status"],"additionalProperties":false}) }
pub fn get_goal_schema()->Value { json!({"type":"object","properties":{},"additionalProperties":false}) }
pub fn parse_model_goal_update(params:&Value)->Result<(ModelSettableGoalStatus,Option<String>),ToolError> {
    let status=match params["status"].as_str() { Some("complete")=>ModelSettableGoalStatus::Complete,Some("blocked")=>ModelSettableGoalStatus::Blocked,_=>return Err(ToolError::Message("status must be complete or blocked".into())) };
    let reason=params.get("reason").and_then(Value::as_str).map(|reason|reason.trim_matches(crate::validation::js_whitespace).to_owned());
    match status {
        ModelSettableGoalStatus::Blocked if reason.as_ref().is_none_or(String::is_empty)=>return Err(ToolError::Message("reason is required when status is blocked".into())),
        ModelSettableGoalStatus::Complete if reason.as_ref().is_some_and(|reason|!reason.is_empty())=>return Err(ToolError::Message("reason must not be provided when status is complete".into())),
        ModelSettableGoalStatus::Complete|ModelSettableGoalStatus::Blocked=>{},
    }
    Ok((status,reason))
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn blocked_update_requires_nonblank_reason() { assert!(parse_model_goal_update(&json!({"status":"blocked","reason":"\u{feff} "})).is_err()); }
    #[test] fn complete_update_rejects_nonblank_reason() { assert!(parse_model_goal_update(&json!({"status":"complete","reason":"blocked"})).is_err()); }
    #[test] fn model_cannot_pause_or_resume_goal() { for status in ["active","paused"] { assert!(parse_model_goal_update(&json!({"status":status})).is_err()); } }
    #[test] fn admitted_blocked_reason_uses_js_trimming() { assert_eq!(parse_model_goal_update(&json!({"status":"blocked","reason":"\u{feff}waiting "})).unwrap(),(ModelSettableGoalStatus::Blocked,Some("waiting".into()))); }
}
