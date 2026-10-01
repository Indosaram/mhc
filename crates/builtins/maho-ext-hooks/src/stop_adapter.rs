use serde_json::{Value,json};
pub const STOP_STATE_CUSTOM_TYPE:&str="senpi.hooks.stop-state";
pub const STOP_DIAGNOSTICS_CUSTOM_TYPE:&str="senpi.hooks.stop-diagnostics";
pub const STOP_OUTPUT_CUSTOM_TYPE:&str="senpi.hooks.stop-output";
pub const STOP_REENTRY_LIMIT:usize=8;
pub fn apply_stop_hook_result(api:&maho_ext_api::types::ExtensionApi,ctx:&maho_ext_api::types::ExtensionContext,result:&crate::dispatcher::HookDispatchResult,turn_key:&str)->Result<(),maho_ext_api::types::ExtensionFailure> {
    use maho_ext_api::types::*;use crate::dispatcher::HookDispatchDecision;
    let session_id=ctx.session_manager.session_id();
    let previous=ctx.session_manager.get_entries().iter().rev().find_map(|entry| {
        if entry.kind!="custom"||entry.data.get("customType").and_then(Value::as_str)!=Some(STOP_STATE_CUSTOM_TYPE) {return None;}
        let state=entry.data.get("data")?;if state.get("sessionId").and_then(Value::as_str)!=Some(session_id)||state.get("turnKey").and_then(Value::as_str)!=Some(turn_key) {return None;}state.get("count").and_then(Value::as_u64)
    }).unwrap_or(0);
    if previous>=STOP_REENTRY_LIMIT as u64 {api.append_entry(STOP_STATE_CUSTOM_TYPE,Some(json!({"count":previous,"sessionId":session_id,"turnKey":turn_key})))?;return Ok(());}
    if !result.diagnostics.is_empty() {api.append_entry(STOP_DIAGNOSTICS_CUSTOM_TYPE,Some(json!(result.diagnostics.iter().map(crate::prompt_adapter::safe_diagnostic_details).collect::<Vec<_>>())))?;}
    let blocked=matches!(result.decision,HookDispatchDecision::Block {..});api.append_entry(STOP_STATE_CUSTOM_TYPE,Some(json!({"count":previous+u64::from(blocked),"sessionId":session_id,"turnKey":turn_key})))?;
    let HookDispatchDecision::Block {source,reason,..}=&result.decision else {return Ok(());};
    let blocker=result.summaries.iter().find(|summary|summary.handler.source.source_path==source.source_path&&matches!(summary.output.get("decision").and_then(Value::as_str),Some("block"|"deny")));
    let Some(blocker)=blocker.filter(|summary|summary.run.exit_code!=Some(2)) else {return Ok(());};
    let follow_up=blocker.output.get("additionalContext").and_then(Value::as_str).or(reason.as_deref());
    if let Some(text)=follow_up {api.send_user_message(UserMessageContent::Text(text.to_owned()),SendUserMessageOptions {deliver_as:Some(StreamingBehavior::FollowUp),expand_prompt_templates:false})?;}
    Ok(())
}
#[derive(Default)]
pub struct StopTurnTracker {active_turn_key:Option<String>,turn_index:usize}
impl StopTurnTracker {
    pub fn reset(&mut self) {self.turn_index+=1;self.active_turn_key=None;}
    pub fn turn_key(&mut self,leaf_id:Option<&str>,session_id:&str)->String {self.active_turn_key.get_or_insert_with(||format!("{}:{}",self.turn_index,leaf_id.unwrap_or(session_id))).clone()}
}
pub fn build_stop_hook_input(messages:&[Value],cwd:&str,session_id:&str,transcript_path:Option<&str>)->Value {
    let mut input=json!({"cwd":cwd,"event":"Stop","hook_event_name":"Stop","session_id":session_id});
    if let Some(reason)=messages.iter().rev().find(|message|message.get("role").and_then(Value::as_str)==Some("assistant")).and_then(|message|message.get("stopReason")) {input["stopReason"]=reason.clone();}
    if let Some(path)=transcript_path {input["transcript_path"]=json!(path);}
    input
}
#[cfg(test)]
mod tests {use super::*;#[test] fn turn_key_is_stable_until_reset() {let mut tracker=StopTurnTracker::default();assert_eq!(tracker.turn_key(Some("leaf"),"s"),"0:leaf");assert_eq!(tracker.turn_key(Some("next"),"s"),"0:leaf");tracker.reset();assert_eq!(tracker.turn_key(None,"s"),"1:s");}#[test] fn last_assistant_only() {let input=build_stop_hook_input(&[json!({"role":"assistant","stopReason":"stop"}),json!({"role":"user","stopReason":"ignored"})],"/repo","s",None);assert_eq!(input["stopReason"],"stop");assert!(input.get("transcript_path").is_none());}}
