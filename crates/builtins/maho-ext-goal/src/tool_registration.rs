use crate::types::ModelSettableGoalStatus;
use maho_ext_api::ToolError;
use serde_json::{Value,json};
pub fn create_goal_schema()->Value { json!({"type":"object","properties":{"objective":{"type":"string","description":"Required. The concrete objective to start pursuing. Limit: 4,000 characters. For longer instructions, put the full objective in a file and refer to that file."}},"required":["objective"],"additionalProperties":false}) }
pub fn update_goal_schema()->Value { json!({"type":"object","properties":{"status":{"anyOf":[{"const":"complete","type":"string"},{"const":"blocked","type":"string"}],"description":"Required. Set to complete when achieved or blocked with a non-empty reason."},"reason":{"type":"string","description":"Required and non-empty when status is blocked; rejected when status is complete."}},"required":["status"],"additionalProperties":false}) }
pub fn get_goal_schema()->Value { json!({"type":"object","properties":{},"additionalProperties":false}) }
pub fn goal_tool_result(goal:Option<&crate::types::Goal>,notice:Option<&str>)->Result<maho_ext_api::ToolResult,ToolError> {
    let text=crate::format::format_goal_tool_response(goal,notice).map_err(|error|ToolError::Message(error.to_string()))?;
    let mut details=serde_json::to_value(crate::format::goal_tool_response(goal)).map_err(|error|ToolError::Message(error.to_string()))?;
    if let Some(notice)=notice { details["notice"]=notice.into(); }
    Ok(maho_ext_api::ToolResult { content:vec![maho_ext_api::ToolContent::text(text)],details:Some(details) })
}
pub async fn execute_create_goal(reference:&crate::types::GoalStoreRef,objective:&str,now:u64)->Result<(crate::types::Goal,maho_ext_api::ToolResult),ToolError> {
    let current=crate::store::read_goal(reference).map_err(|error|ToolError::Message(error.to_string()))?;
    if current.is_some_and(|goal|goal.status!=crate::types::GoalStatus::Complete) { return Err(ToolError::Message("cannot create a new goal because this thread already has an unfinished goal; use update_goal only when the existing goal is complete".into())); }
    let file=crate::store::objective_full_text_file_name(reference);
    let validated=crate::validation::validate_objective(objective,&file).map_err(|error|ToolError::Message(error.to_string()))?;
    let goal=crate::store::create_goal(reference,objective,None,now).await.map_err(|error|ToolError::Message(error.to_string()))?;
    let notice=validated.truncated.then(||crate::validation::objective_truncation_notice(&file));
    let result=goal_tool_result(Some(&goal),notice.as_deref())?; Ok((goal,result))
}
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
    #[tokio::test] async fn create_tool_returns_snapshot_and_rejects_unfinished_replacement() {
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        let (goal,result)=execute_create_goal(&reference,"work",0).await.unwrap(); assert_eq!(result.details.unwrap()["goal"]["objective"],goal.objective);
        assert!(execute_create_goal(&reference,"replacement",1).await.is_err()); assert_eq!(crate::store::read_goal(&reference).unwrap().unwrap().id,goal.id);
    }
    #[tokio::test] async fn truncated_create_tool_returns_notice_and_full_objective_sidecar() {
        let dir=tempfile::tempdir().unwrap(); let reference=crate::types::GoalStoreRef { base_dir:dir.path().into(),thread_id:"s".into() };
        let objective="x".repeat(4001); let (_,result)=execute_create_goal(&reference,&objective,0).await.unwrap();
        assert!(result.details.unwrap()["notice"].is_string()); assert_eq!(std::fs::read_to_string(crate::store::objective_full_text_file_path(&reference)).unwrap(),objective);
    }
    #[test] fn blocked_update_requires_nonblank_reason() { assert!(parse_model_goal_update(&json!({"status":"blocked","reason":"\u{feff} "})).is_err()); }
    #[test] fn complete_update_rejects_nonblank_reason() { assert!(parse_model_goal_update(&json!({"status":"complete","reason":"blocked"})).is_err()); }
    #[test] fn model_cannot_pause_or_resume_goal() { for status in ["active","paused"] { assert!(parse_model_goal_update(&json!({"status":status})).is_err()); } }
    #[test] fn admitted_blocked_reason_uses_js_trimming() { assert_eq!(parse_model_goal_update(&json!({"status":"blocked","reason":"\u{feff}waiting "})).unwrap(),(ModelSettableGoalStatus::Blocked,Some("waiting".into()))); }
}
