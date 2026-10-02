use crate::dispatcher::{HookDispatchDecision,HookDispatchResult};
use crate::types::HookDiagnostic;
use serde_json::{Value,json};
pub const HOOK_CUSTOM_MESSAGE_TYPE:&str="senpi.hook";
pub const USER_PROMPT_BLOCK_REASON:&str="UserPromptSubmit hook blocked the prompt.";
pub struct UserPromptHookInputOptions<'a> {pub cwd:&'a str,pub permission_mode:&'a str,pub prompt:&'a str,pub session_id:&'a str,pub transcript_path:Option<&'a str>}
pub struct PendingPromptHookContext {pub additional_context:Vec<String>,pub diagnostics:Vec<HookDiagnostic>,pub system_messages:Vec<String>}
pub fn build_user_prompt_hook_input(options:UserPromptHookInputOptions<'_>)->Value {let mut input=json!({"cwd":options.cwd,"event":"UserPromptSubmit","permission_mode":options.permission_mode,"prompt":options.prompt,"session_id":options.session_id});if let Some(path)=options.transcript_path {input["transcript_path"]=json!(path);}input}
pub fn prompt_context_from_result(result:&HookDispatchResult)->Option<PendingPromptHookContext> {
    let mut additional_context=Vec::new();let mut system_messages=Vec::new();
    for summary in &result.summaries {
        if let Some(context)=summary.output.get("additionalContext").and_then(Value::as_str) {additional_context.push(context.to_owned());}
        if let Some(message)=summary.output.get("systemMessage").and_then(Value::as_str) {system_messages.push(message.to_owned());}
    }
    if additional_context.is_empty()&&system_messages.is_empty()&&result.diagnostics.is_empty() {None} else {Some(PendingPromptHookContext {additional_context,system_messages,diagnostics:result.diagnostics.clone()})}
}
pub fn prompt_block_reason_from_result(result:&HookDispatchResult)->String {let HookDispatchDecision::Block {source,reason,..}=&result.decision else {return USER_PROMPT_BLOCK_REASON.to_owned();};let blocker=result.summaries.iter().find(|summary|summary.handler.source.source_path==source.source_path&&matches!(summary.output.get("decision").and_then(Value::as_str),Some("block"|"deny")));if blocker.is_some_and(|blocker|blocker.run.exit_code==Some(2)) {USER_PROMPT_BLOCK_REASON.to_owned()} else {reason.clone().unwrap_or_else(||USER_PROMPT_BLOCK_REASON.to_owned())}}
pub fn format_prompt_context_message(pending:&PendingPromptHookContext)->Option<String> {if pending.additional_context.is_empty() {None} else {Some(pending.additional_context.join("\n\n"))}}
pub fn append_system_messages(system_prompt:&str,messages:&[String])->String {if messages.is_empty() {system_prompt.to_owned()} else {format!("{system_prompt}\n\n{}",messages.join("\n\n"))}}
pub fn safe_diagnostic_details(diagnostic:&HookDiagnostic)->Value {let mut details=json!({"code":diagnostic.code,"message":diagnostic.message,"path":diagnostic.path,"severity":diagnostic.severity,"sourcePath":diagnostic.source.source_path});if let Some(event)=&diagnostic.event {details["event"]=json!(event);}details}
