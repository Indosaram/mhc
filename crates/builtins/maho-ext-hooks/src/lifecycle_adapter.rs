use serde_json::{Value,json};
use crate::dispatcher::{HookDispatchResult,HookDispatchDecision};
use crate::types::{HookDiagnostic,Severity};
use crate::diagnostics::{diagnostic,DiagnosticDraft};
#[derive(Default)]
pub struct LifecycleResultDetails {pub cancel:bool,pub contexts:Vec<String>,pub diagnostics:Vec<HookDiagnostic>,pub reason:Option<String>}
pub fn lifecycle_result_details(event:&str,result:Option<&HookDispatchResult>)->LifecycleResultDetails {
    let Some(result)=result else {return LifecycleResultDetails::default();};
    let mut details=LifecycleResultDetails {diagnostics:result.diagnostics.clone(),..Default::default()};
    for summary in &result.summaries {
        let code=summary.run.exit_code;
        if code!=Some(0)&&!(code==Some(2)&&event=="PreCompact") {details.diagnostics.push(diagnostic(DiagnosticDraft {code:"invalid_root",event:Some(event),message:format!("Hook command failed {}.",code.map(|code|format!("with exit code {code}")).unwrap_or_else(||"without an exit code".to_owned())),path:"process.exitCode".to_owned(),severity:Some(Severity::Warning)},&summary.handler.source));}
        let Ok(raw)=serde_json::from_str::<Value>(summary.run.stdout.trim()) else {continue;};let Some(raw)=raw.as_object() else {continue;};let specific=raw.get("hookSpecificOutput").and_then(Value::as_object);
        if specific.and_then(|raw|raw.get("hookEventName")).is_some_and(|name|name.as_str()!=Some(event)) {continue;}
        let field=|name:&str|specific.and_then(|raw|raw.get(name)).filter(|value|!value.is_null()).or_else(||raw.get(name)).and_then(Value::as_str).map(str::trim).filter(|text|!text.is_empty());
        if event=="Notification" {if let Some(context)=summary.output.get("additionalContext").and_then(Value::as_str) {details.contexts.push(context.to_owned());}}
        else if event!="PreCompact" {if let Some(context)=field("additionalContext") {details.contexts.push(context.to_owned());}}
        else {
            for (name,message) in [("additionalContext","PreCompact additionalContext is diagnostic-only in builtin hooks v1."),("customInstructions","PreCompact customInstructions cannot mutate compaction in builtin hooks v1.")] {if field(name).is_some() {details.diagnostics.push(diagnostic(DiagnosticDraft {code:"unsupported_field",event:Some(event),message:message.to_owned(),path:format!("stdout.hookSpecificOutput.{name}"),severity:Some(Severity::Warning)},&summary.handler.source));}}
            if !details.cancel&&matches!(raw.get("decision").and_then(Value::as_str).map(str::trim),Some("block"|"deny")) {details.cancel=true;details.reason=specific.and_then(|raw|raw.get("permissionDecisionReason")).filter(|value|!value.is_null()).or_else(||raw.get("reason")).and_then(Value::as_str).map(str::trim).filter(|text|!text.is_empty()).map(str::to_owned);}
        }
    }
    if event=="PreCompact" && let HookDispatchDecision::Block {reason,..}=&result.decision {details.cancel=true;details.reason=reason.clone();}
    details
}
pub fn lifecycle_message(event:&str,details:&LifecycleResultDetails,request_id:Option<&str>)->Option<maho_ext_api::types::CustomMessage> {
    if details.contexts.is_empty()&&details.diagnostics.is_empty()&&details.reason.is_none() {return None;}
    let text=if !details.contexts.is_empty() {details.contexts.join("\n\n")} else {details.reason.clone().unwrap_or_else(||format!("{event} hook diagnostics."))};
    let mut data=json!({"event":event,"diagnostics":details.diagnostics.iter().map(crate::prompt_adapter::safe_diagnostic_details).collect::<Vec<_>>()});if let Some(id)=request_id {data["compactionRequestId"]=json!(id);}
    Some(maho_ext_api::types::CustomMessage {custom_type:crate::prompt_adapter::HOOK_CUSTOM_MESSAGE_TYPE.to_owned(),content:vec![maho_ext_api::types::ToolContent::text(text)],display:false,details:Some(data)})
}
pub struct LifecycleInputContext<'a> {pub cwd:&'a str,pub session_id:&'a str,pub transcript_path:Option<&'a str>}
fn base(event:&str,context:&LifecycleInputContext<'_>)->Value {let mut input=json!({"cwd":context.cwd,"event":event,"hook_event_name":event,"session_id":context.session_id});if let Some(path)=context.transcript_path {input["transcript_path"]=json!(path);}input}
pub fn build_session_start_hook_input(reason:&str,context:&LifecycleInputContext<'_>)->Value {let mut input=base("SessionStart",context);input["reason"]=json!(reason);input["sessionId"]=json!(context.session_id);input}
pub fn build_pre_compact_hook_input(reason:&str,request_id:&str,will_retry:bool,custom_instructions:Option<&str>,context:&LifecycleInputContext<'_>)->Value {let mut input=base("PreCompact",context);input["reason"]=json!(reason);input["request_id"]=json!(request_id);input["will_retry"]=json!(will_retry);if let Some(instructions)=custom_instructions {input["custom_instructions"]=json!(instructions);}input}
pub fn build_post_compact_hook_input(reason:&str,request_id:&str,will_retry:bool,accepted:bool,context:&LifecycleInputContext<'_>)->Value {let mut input=base("PostCompact",context);input["reason"]=json!(reason);input["request_id"]=json!(request_id);input["will_retry"]=json!(will_retry);input["accepted"]=json!(accepted);input}
pub struct NotificationHookInput<'a> {pub message:&'a str,pub kind:&'a str,pub title:Option<&'a str>,pub source:Option<&'a str>,pub request_id:Option<&'a str>,pub status:Option<&'a str>}
pub fn build_notification_hook_input(notification:NotificationHookInput<'_>,context:&LifecycleInputContext<'_>)->Value {let mut input=base("Notification",context);input["kind"]=json!(notification.kind);input["message"]=json!(notification.message);for (key,value) in [("title",notification.title),("notification_source",notification.source),("request_id",notification.request_id),("status",notification.status)] {if let Some(value)=value {input[key]=json!(value);}}input}
#[cfg(test)]
mod tests {use super::*;#[test] fn builds_lifecycle_wire_with_optional_fields() {let context=LifecycleInputContext {cwd:"/repo",session_id:"s",transcript_path:None};let start=build_session_start_hook_input("startup",&context);assert_eq!(start["sessionId"],start["session_id"]);assert!(start.get("transcript_path").is_none());let compact=build_pre_compact_hook_input("manual","r",true,Some("brief"),&context);assert_eq!(compact["will_retry"],true);assert_eq!(compact["custom_instructions"],"brief");let post=build_post_compact_hook_input("manual","r",false,true,&context);assert_eq!(post["accepted"],true);}}

#[cfg(test)]
mod adapter_tests {
    use super::*;
    use crate::command_runner::{CommandHookRunOptions,CommandHookRunResult,run_command_hook};
    use crate::dispatcher::{HookDispatchDecision,HookDispatchResult,HookDispatchSummary};
    use crate::types::{CommandHookConfig,ExecutableHookHandler,HookDiscoveryTiming,HookSourceScope,SupportedHookEvent};
    fn handler(event:SupportedHookEvent)->ExecutableHookHandler {
        ExecutableHookHandler {event,matcher:None,group_index:0,handler_index:0,config:CommandHookConfig {kind:"command".to_owned(),command:"exit 0".to_owned(),command_windows:None,timeout:None,status_message:None},source:HookSourceMetadata {scope:HookSourceScope::Project,source_path:"/repo/hooks.json".to_owned(),display_order:0,discovered_at:HookDiscoveryTiming::PreSession,plugin_root:None,manifest_path:None,plugin_env:None}}
    }
    async fn run_result(stdout:&str,stderr:&str,exit_code:i32)->CommandHookRunResult {
        let options=CommandHookRunOptions {cwd:std::path::Path::new("/tmp"),env_passthrough:&[],output_policy:None,signal:None,source_env:None};
        let mut result=run_command_hook(&handler(SupportedHookEvent::PostCompact),&json!({"event":"PostCompact"}),options).await.unwrap();
        result.stdout=stdout.to_owned();result.stderr=stderr.to_owned();result.exit_code=Some(exit_code);result
    }
    async fn dispatch(event:SupportedHookEvent,stdout:&str,stderr:&str,exit_code:i32)->HookDispatchResult {
        let output=serde_json::from_str::<Value>(stdout).ok().and_then(|value|value.as_object().cloned()).unwrap_or_default();
        HookDispatchResult {decision:HookDispatchDecision::None,diagnostics:vec![],executable_handlers:vec![],matched_handlers:vec![],skipped:vec![],summaries:vec![HookDispatchSummary {completion_index:0,diagnostics:vec![],handler:handler(event),output,run:run_result(stdout,stderr,exit_code).await}]}
    }
    #[tokio::test]
    async fn pre_compact_context_and_instructions_are_diagnostic_only() {
        let result=dispatch(SupportedHookEvent::PreCompact,r#"{"hookSpecificOutput":{"hookEventName":"PreCompact","additionalContext":"ctx","customInstructions":"brief"}}"#,"",0).await;
        let details=lifecycle_result_details("PreCompact",Some(&result));
        assert!(details.contexts.is_empty());assert!(!details.cancel);
        assert_eq!(details.diagnostics.iter().map(|diagnostic|diagnostic.path.clone()).collect::<Vec<_>>(),vec!["stdout.hookSpecificOutput.additionalContext".to_owned(),"stdout.hookSpecificOutput.customInstructions".to_owned()]);
        assert!(details.diagnostics.iter().all(|diagnostic|diagnostic.code=="unsupported_field"));
    }
    #[tokio::test]
    async fn post_compact_exit_two_reports_sanitized_diagnostic_without_secret_stderr() {
        let result=dispatch(SupportedHookEvent::PostCompact,"","SECRET_TWO",2).await;
        let details=lifecycle_result_details("PostCompact",Some(&result));
        assert!(details.contexts.is_empty());assert!(!details.cancel);
        assert!(details.diagnostics.iter().any(|diagnostic|diagnostic.code=="invalid_root"&&diagnostic.message.contains("exit code 2")));
        assert!(!details.diagnostics.iter().any(|diagnostic|diagnostic.message.contains("SECRET")));
    }
}
