use serde_json::{Value,json};
pub const STOP_STATE_CUSTOM_TYPE:&str="senpi.hooks.stop-state";
pub const STOP_DIAGNOSTICS_CUSTOM_TYPE:&str="senpi.hooks.stop-diagnostics";
pub const STOP_OUTPUT_CUSTOM_TYPE:&str="senpi.hooks.stop-output";
pub const STOP_REENTRY_LIMIT:usize=8;
pub async fn apply_stop_hook_result(api:&maho_ext_api::types::ExtensionApi,ctx:&maho_ext_api::types::ExtensionContext,result:&crate::dispatcher::HookDispatchResult,turn_key:&str)->Result<(),maho_ext_api::types::ExtensionFailure> {
    use maho_ext_api::types::*;use crate::dispatcher::HookDispatchDecision;
    let session_id=ctx.session_manager.session_id();
    let previous=ctx.session_manager.get_entries().iter().rev().find_map(|entry| {
        if entry.kind!="custom"||entry.data.get("customType").and_then(Value::as_str)!=Some(STOP_STATE_CUSTOM_TYPE) {return None;}
        let state=entry.data.get("data")?;if state.get("sessionId").and_then(Value::as_str)!=Some(session_id)||state.get("turnKey").and_then(Value::as_str)!=Some(turn_key) {return None;}state.get("count").and_then(Value::as_u64)
    }).unwrap_or(0);
    if previous>=STOP_REENTRY_LIMIT as u64 {
        api.append_entry(STOP_STATE_CUSTOM_TYPE,Some(json!({"count":previous,"sessionId":session_id,"turnKey":turn_key})))?;
        let source=result.matched_handlers.first().or(result.executable_handlers.first()).map(|handler|handler.source.clone()).unwrap_or(crate::types::HookSourceMetadata {scope:crate::types::HookSourceScope::Managed,source_path:"<builtin:hooks>".to_owned(),display_order:0,discovered_at:crate::types::HookDiscoveryTiming::Runtime,plugin_root:None,manifest_path:None,plugin_env:None});
        api.append_entry(STOP_DIAGNOSTICS_CUSTOM_TYPE,Some(json!([stop_diagnostic(&source,"unsupported_event","hooks.Stop","Stop hook reentry limit reached.")])))?;
        return Ok(());
    }
    let mut diagnostics=result.diagnostics.iter().map(|diagnostic| {
        let mut details=crate::prompt_adapter::safe_diagnostic_details(diagnostic);
        if diagnostic.code=="invalid_event_config"&&diagnostic.event.as_deref()==Some("Stop")&&diagnostic.path=="stdout.hookSpecificOutput.hookEventName" {details["message"]=json!("Hook output event does not match Stop.");}
        details
    }).collect::<Vec<_>>();
    let mut records=vec![];
    for summary in &result.summaries {
        let (fields,unsupported)=stop_output_details(&summary.output,&summary.run.stdout);
        diagnostics.extend(unsupported.into_iter().map(|(path,message)|stop_diagnostic(&summary.handler.source,"unsupported_field",&path,&message)));
        if !fields.is_empty() {records.push(json!({"event":"Stop","exitCode":summary.run.exit_code,"fields":fields,"sourcePath":summary.handler.source.source_path}));}
    }
    if !diagnostics.is_empty() {api.append_entry(STOP_DIAGNOSTICS_CUSTOM_TYPE,Some(json!(diagnostics)))?;}
    if !records.is_empty() {api.append_entry(STOP_OUTPUT_CUSTOM_TYPE,Some(json!(records)))?;}
    let blocked=matches!(result.decision,HookDispatchDecision::Block {..});api.append_entry(STOP_STATE_CUSTOM_TYPE,Some(json!({"count":previous+u64::from(blocked),"sessionId":session_id,"turnKey":turn_key})))?;
    let HookDispatchDecision::Block {source,reason,..}=&result.decision else {return Ok(());};
    let blocker=result.summaries.iter().find(|summary|summary.handler.source.source_path==source.source_path&&matches!(summary.output.get("decision").and_then(Value::as_str),Some("block"|"deny")));
    let follow_up=blocker.filter(|summary|summary.run.exit_code!=Some(2)).and_then(|summary|summary.output.get("additionalContext").and_then(Value::as_str).or(reason.as_deref()));
    if let Some(text)=follow_up {
        api.send_user_message(UserMessageContent::Text(text.to_owned()),SendUserMessageOptions {deliver_as:Some(StreamingBehavior::FollowUp),expand_prompt_templates:false})?;
        for _ in 0..64 {tokio::task::yield_now().await;if ctx.has_pending_messages()? {break;}}
    }
    else {api.append_entry(STOP_DIAGNOSTICS_CUSTOM_TYPE,Some(json!([stop_diagnostic(source,"unsupported_field","stdout.reason","Stop hook blocked without follow-up context.")])))?;}
    Ok(())
}
fn stop_diagnostic(source:&crate::types::HookSourceMetadata,code:&str,path:&str,message:&str)->Value {
    crate::prompt_adapter::safe_diagnostic_details(&crate::diagnostics::diagnostic(crate::diagnostics::DiagnosticDraft {code,message:message.to_owned(),path:path.to_owned(),event:Some("Stop"),severity:Some(crate::types::Severity::Warning)},source))
}
fn stop_output_details(output:&serde_json::Map<String,Value>,stdout:&str)->(Vec<String>,Vec<(String,String)>) {
    let raw=serde_json::from_str::<Value>(stdout.trim()).ok();
    let specific=raw.as_ref().and_then(|value|value.get("hookSpecificOutput")).and_then(Value::as_object);
    let nested_context=specific.is_some_and(|value|value.contains_key("additionalContext"));
    let mut fields=vec![];
    for key in ["decision","reason","additionalContext","continue","stopReason","suppressOutput","systemMessage"] {
        if output.contains_key(key)||(key=="additionalContext"&&nested_context) {fields.push(if key=="additionalContext"&&nested_context {"stdout.hookSpecificOutput.additionalContext".to_owned()} else {format!("stdout.{key}")});}
    }
    let mut unsupported=vec![];
    for (key,message) in [("systemMessage","Stop does not support systemMessage."),("suppressOutput","Stop does not support suppressOutput."),("stopReason","Stop output stopReason is diagnostic-only."),("updatedInput","Stop does not support updatedInput."),("updatedToolOutput","Stop does not support updatedToolOutput.")] {
        if output.contains_key(key) {unsupported.push((format!("stdout.{key}"),message.to_owned()));}
    }
    for (prefix,record) in [("stdout",raw.as_ref().and_then(Value::as_object)),("stdout.hookSpecificOutput",specific)] {
        for key in ["updatedInput","updatedToolOutput"] {
            if record.is_some_and(|record|record.contains_key(key)) {
                fields.push(format!("{prefix}.{key}"));
                let field=format!("{prefix}.{key}");
                unsupported.push((field.clone(),format!("Stop does not support {}.",field.trim_start_matches("stdout."))));
            }
        }
    }
    (fields,unsupported)
}
#[cfg(test)]
mod output_tests {
    use super::*;
    #[test]
    fn output_records_retain_nested_context_and_unsupported_field_paths() {
        let output=json!({"decision":"block","additionalContext":"follow up","systemMessage":"unsupported"});
        let (fields,diagnostics)=stop_output_details(output.as_object().unwrap(),r#"{"updatedInput":{},"hookSpecificOutput":{"additionalContext":"follow up","updatedToolOutput":{}}}"#);
        assert_eq!(fields,["stdout.decision","stdout.hookSpecificOutput.additionalContext","stdout.systemMessage","stdout.updatedInput","stdout.hookSpecificOutput.updatedToolOutput"]);
        assert_eq!(diagnostics,vec![("stdout.systemMessage".to_owned(),"Stop does not support systemMessage.".to_owned()),("stdout.updatedInput".to_owned(),"Stop does not support updatedInput.".to_owned()),("stdout.hookSpecificOutput.updatedToolOutput".to_owned(),"Stop does not support hookSpecificOutput.updatedToolOutput.".to_owned())]);
    }
    #[test]
    fn nonblocking_output_records_fields_without_unsupported_diagnostics() {
        let raw=r#"{"reason":"gate","hookSpecificOutput":{"hookEventName":"Stop","additionalContext":"ctx"}}"#;
        let (fields,diagnostics)=stop_output_details(serde_json::from_str::<Value>(raw).unwrap().as_object().unwrap(),raw);
        assert_eq!(fields,["stdout.reason","stdout.hookSpecificOutput.additionalContext"]);assert!(diagnostics.is_empty());
    }
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
mod tests {use super::*;#[test] fn turn_key_is_stable_until_reset() {let mut tracker=StopTurnTracker::default();assert_eq!(tracker.turn_key(Some("leaf"),"s"),"0:leaf");assert_eq!(tracker.turn_key(Some("next"),"s"),"0:leaf");tracker.reset();assert_eq!(tracker.turn_key(None,"s"),"1:s");}#[test] fn last_assistant_only() {let input=build_stop_hook_input(&[json!({"role":"assistant","stopReason":"stop"}),json!({"role":"user","stopReason":"ignored"})],"/repo","s",None);assert_eq!(input["stopReason"],"stop");assert!(input.get("transcript_path").is_none());}
#[test] fn stop_input_carries_event_naming_and_transcript() {let input=build_stop_hook_input(&[json!({"role":"assistant","stopReason":"stop"})],"/repo","s",Some("/t.jsonl"));assert_eq!(input["event"],"Stop");assert_eq!(input["hook_event_name"],"Stop");assert_eq!(input["cwd"],"/repo");assert_eq!(input["transcript_path"],"/t.jsonl");}}
