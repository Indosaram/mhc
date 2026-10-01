use serde_json::{Map,Value};
use crate::command_runner::CommandHookRunResult;
use crate::types::{ExecutableHookHandler,HookDiagnostic,HookSourceMetadata,SupportedHookEvent,HookTrustState,Severity};
use crate::matcher::{matching_hook_handlers,HookMatcherInput};
use crate::trust::{HookTrustRecord,list_hook_trust_records};
use crate::safety::validate_hook_handler_safety;
use crate::output_parser::{parse_hook_output,HookOutputParseInput};

#[derive(Debug,PartialEq)]
pub enum HookDispatchDecision {
    None,
    Allow {source:HookSourceMetadata,source_command:String,updated_input:Option<Value>},
    Block {source:HookSourceMetadata,source_command:String,reason:Option<String>},
    Ask {source:HookSourceMetadata,source_command:String,reason:Option<String>,fallback_reason:String},
}
pub struct HookDispatchSummary {pub completion_index:usize,pub diagnostics:Vec<HookDiagnostic>,pub handler:ExecutableHookHandler,pub output:Map<String,Value>,pub run:CommandHookRunResult}
pub struct HookDispatchSkipped {pub diagnostics:Vec<HookDiagnostic>,pub handler:ExecutableHookHandler,pub reason:&'static str,pub record:HookTrustRecord}
pub struct HookDispatchResult {pub decision:HookDispatchDecision,pub diagnostics:Vec<HookDiagnostic>,pub executable_handlers:Vec<ExecutableHookHandler>,pub matched_handlers:Vec<ExecutableHookHandler>,pub skipped:Vec<HookDispatchSkipped>,pub summaries:Vec<HookDispatchSummary>}

pub async fn dispatch_hook_event<F,Fut>(handlers:&[ExecutableHookHandler],input:&Value,trust_state:&HookTrustState,platform:&str,runner:F)->std::io::Result<HookDispatchResult>
where F:Fn(ExecutableHookHandler)->Fut,Fut:std::future::Future<Output=std::io::Result<CommandHookRunResult>> {
    dispatch_hook_event_with_status(handlers,input,trust_state,platform,runner,|_|{}).await
}
pub async fn dispatch_hook_event_with_status<F,Fut,S>(handlers:&[ExecutableHookHandler],input:&Value,trust_state:&HookTrustState,platform:&str,runner:F,mut status:S)->std::io::Result<HookDispatchResult>
where F:Fn(ExecutableHookHandler)->Fut,Fut:std::future::Future<Output=std::io::Result<CommandHookRunResult>>,S:FnMut(&[ExecutableHookHandler]) {
    let event:SupportedHookEvent=serde_json::from_value(input.get("event").cloned().unwrap_or(Value::Null)).map_err(std::io::Error::other)?;
    let matched=matching_hook_handlers(HookMatcherInput {event,tool_name:input.get("toolName").and_then(Value::as_str).unwrap_or("")},handlers);
    let mut matched_handlers=matched.handlers.into_iter().cloned().collect::<Vec<_>>();
    matched_handlers.sort_by_key(|handler|(handler.source.display_order,handler.group_index,handler.handler_index));
    let records=list_hook_trust_records(&matched_handlers,trust_state,platform).map_err(std::io::Error::other)?;
    let mut skipped=Vec::new();let mut executable_handlers=Vec::new();
    for (handler,record) in matched_handlers.iter().zip(records) {
        let diagnostics=validate_hook_handler_safety(handler)?;
        let reason=if diagnostics.iter().any(|d|d.severity==Severity::Error) {Some("unsafe")} else if !record.enabled {Some("disabled")} else if !record.trusted {Some("untrusted")} else {None};
        if let Some(reason)=reason {skipped.push(HookDispatchSkipped {diagnostics:if reason=="unsafe" {diagnostics} else {Vec::new()},handler:handler.clone(),reason,record});} else {executable_handlers.push(handler.clone());}
    }
    let mut pending=futures::stream::FuturesUnordered::new();
    let mut running=Vec::new();
    for (index,handler) in executable_handlers.iter().cloned().enumerate() {running.push((index,handler.clone()));status(&running.iter().map(|(_,handler)|handler.clone()).collect::<Vec<_>>());let future=runner(handler.clone());pending.push(async move {(index,handler,future.await)});}
    use futures::StreamExt;
    let mut completed=Vec::new();
    while let Some((declaration_index,handler,run))=pending.next().await {running.retain(|(index,_)|*index!=declaration_index);status(&running.iter().map(|(_,handler)|handler.clone()).collect::<Vec<_>>());let run=run?;let parsed=parse_hook_output(HookOutputParseInput {event,exit_code:run.exit_code.unwrap_or(1),stdout:&run.stdout,stderr:&run.stderr,source:&handler.source});let completion_index=completed.len();completed.push((declaration_index,HookDispatchSummary {completion_index,diagnostics:parsed.diagnostics,handler,output:parsed.output,run}));}
    completed.sort_by_key(|(index,_)|*index);let summaries=completed.into_iter().map(|(_,summary)|summary).collect::<Vec<_>>();
    let mut diagnostics=matched.diagnostics;diagnostics.extend(skipped.iter().flat_map(|s|s.diagnostics.iter().cloned()));diagnostics.extend(summaries.iter().flat_map(|s|s.diagnostics.iter().cloned()));
    Ok(HookDispatchResult {decision:aggregate_decision(event,&summaries),diagnostics,executable_handlers,matched_handlers,skipped,summaries})
}

pub fn running_hook_handlers_status_label(handlers:&[ExecutableHookHandler],platform:&str)->String {
    let ansi=fancy_regex::Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))").expect("status ANSI regex");
    let labels=handlers.iter().map(|handler| {
        let raw=handler.config.status_message.as_deref().unwrap_or_else(||crate::plugin_loader::select_hook_command_for_platform(handler,platform));
        let stripped=ansi.replace_all(raw,"");let replaced=stripped.replace(['\r','\n','\t']," ");
        replaced.chars().filter(|character|!matches!(u32::from(*character),0..=31|127)).collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ")
    }).collect::<Vec<_>>().join(" · ");
    let units=labels.encode_utf16().collect::<Vec<_>>();if units.len()>79 {format!("{}...",String::from_utf16_lossy(&units[..76]))} else {labels}
}
pub fn aggregate_decision(event:SupportedHookEvent,summaries:&[HookDispatchSummary])->HookDispatchDecision {
    let decision=|summary:&HookDispatchSummary|summary.output.get("decision").and_then(Value::as_str).map(str::to_owned);
    if let Some(blocker)=summaries.iter().find(|summary|matches!(decision(summary).as_deref(),Some("block"|"deny"))) {
        let reason=blocker.output.get("reason").or_else(||if event==SupportedHookEvent::PreToolUse {None} else {blocker.output.get("stopReason")}).and_then(Value::as_str).map(str::to_owned);
        return HookDispatchDecision::Block {source:blocker.handler.source.clone(),source_command:blocker.handler.config.command.clone(),reason};
    }
    if event!=SupportedHookEvent::PreToolUse {return HookDispatchDecision::None;}
    if let Some(ask)=summaries.iter().find(|summary|decision(summary).as_deref()==Some("ask")) {let reason=ask.output.get("reason").and_then(Value::as_str).map(str::to_owned);return HookDispatchDecision::Ask {source:ask.handler.source.clone(),source_command:ask.handler.config.command.clone(),fallback_reason:reason.clone().unwrap_or_else(||"Hook requested manual approval.".to_owned()),reason};}
    if let Some(allow)=summaries.iter().rev().find(|summary|matches!(decision(summary).as_deref(),Some("allow"|"approve"))) {return HookDispatchDecision::Allow {source:allow.handler.source.clone(),source_command:allow.handler.config.command.clone(),updated_input:allow.output.get("updatedInput").cloned()};}
    HookDispatchDecision::None
}

#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;use crate::schema::parse_hook_config;use crate::types::{HookSourceScope,HookDiscoveryTiming};use crate::trust::{create_hook_trust_entry,hook_trust_id};
    #[tokio::test]
    async fn deferred_completion_keeps_declaration_order_and_deny_precedence()->std::io::Result<()> {
        let handlers=vec![handler("allow-slow",0),handler("deny-fast",1)];let trust=state(&handlers)?;let (started,mut starts)=tokio::sync::mpsc::unbounded_channel();let (status,mut transitions)=tokio::sync::mpsc::unbounded_channel();
        let (slow_sender,slow)=tokio::sync::oneshot::channel();let (fast_sender,fast)=tokio::sync::oneshot::channel();let receivers=std::sync::Arc::new(std::sync::Mutex::new(vec![Some(slow),Some(fast)]));
        let outputs=vec![run(handlers[0].clone(),json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{"command":"safe"}}}).to_string()).await?,run(handlers[1].clone(),json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"dangerous"}}).to_string()).await?];
        let dispatch=tokio::spawn(async move {dispatch_hook_event_with_status(&handlers,&json!({"event":"PreToolUse","toolName":"bash"}),&trust,"linux",move |handler| {started.send(handler.config.command).unwrap();let receiver=receivers.lock().unwrap()[handler.handler_index].take().unwrap();async move {receiver.await.map_err(std::io::Error::other)}},move |running| {status.send(running.iter().map(|handler|handler.config.command.clone()).collect::<Vec<_>>()).unwrap();}).await});
        let result=tokio::time::timeout(std::time::Duration::from_secs(5),async {
            assert_eq!(starts.recv().await.unwrap(),"allow-slow");assert_eq!(starts.recv().await.unwrap(),"deny-fast");assert_eq!(transitions.recv().await.unwrap(),["allow-slow"]);assert_eq!(transitions.recv().await.unwrap(),["allow-slow","deny-fast"]);
            let mut outputs=outputs;fast_sender.send(outputs.pop().unwrap()).ok().unwrap();assert_eq!(transitions.recv().await.unwrap(),["allow-slow"]);slow_sender.send(outputs.pop().unwrap()).ok().unwrap();assert!(transitions.recv().await.unwrap().is_empty());dispatch.await.unwrap()
        }).await.unwrap()?;
        assert_eq!(result.summaries.iter().map(|summary|summary.handler.config.command.as_str()).collect::<Vec<_>>(),["allow-slow","deny-fast"]);assert_eq!(result.summaries.iter().map(|summary|summary.completion_index).collect::<Vec<_>>(),[1,0]);assert!(matches!(result.decision,HookDispatchDecision::Block {reason:Some(ref reason),source_command:ref command,..} if reason=="dangerous"&&command=="deny-fast"));assert!(result.diagnostics.is_empty());Ok(())
    }
    #[tokio::test]
    async fn concurrent_status_tracks_all_starts_and_each_settlement()->std::io::Result<()> {
        let handlers=vec![handler("first",0),handler("second",1)];let mut sizes=Vec::new();
        dispatch_hook_event_with_status(&handlers,&json!({"event":"PreToolUse"}),&state(&handlers)?,"linux",|handler|run(handler,String::new()),|running|sizes.push(running.len())).await?;
        assert_eq!(sizes,vec![1,2,1,0]);Ok(())
    }
    #[test]
    fn status_label_removes_control_sequences_and_bounds_utf16() {
        let mut handler=handler("command",0);handler.config.status_message=Some(format!("\x1b[31m{}\x1b[0m\r\n", "x".repeat(100)));
        let label=running_hook_handlers_status_label(&[handler],"linux");assert_eq!(label.encode_utf16().count(),79);assert!(!label.chars().any(char::is_control));
    }
    fn handler(command:&str,index:usize)->ExecutableHookHandler {let mut handler=parse_hook_config(&json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":command}]}]}}),&HookSourceMetadata {scope:HookSourceScope::Project,source_path:"/repo/hooks.json".to_owned(),display_order:1,discovered_at:HookDiscoveryTiming::PreSession,plugin_root:None,manifest_path:None,plugin_env:None}).executable_handlers.remove(0);handler.handler_index=index;handler}
    fn state(handlers:&[ExecutableHookHandler])->std::io::Result<HookTrustState> {let mut state=HookTrustState {version:1,hooks:Default::default()};for handler in handlers {state.hooks.insert(hook_trust_id(handler),create_hook_trust_entry(handler,"linux","fixed").map_err(std::io::Error::other)?);}Ok(state)}
    async fn run(handler:ExecutableHookHandler,stdout:String)->std::io::Result<CommandHookRunResult> {let options=crate::command_runner::CommandHookRunOptions {cwd:std::path::Path::new("/tmp"),env_passthrough:&[],output_policy:None,signal:None,source_env:None};let mut handler=handler;handler.config.command="exit 0".to_owned();let mut result=crate::command_runner::run_command_hook(&handler,&json!({"event":"PreToolUse"}),options).await?;result.stdout=stdout;Ok(result)}
    #[tokio::test] async fn ask_precedes_allow()->std::io::Result<()> {let handlers=vec![handler("allow",0),handler("ask",1)];let result=dispatch_hook_event(&handlers,&json!({"event":"PreToolUse"}),&state(&handlers)?,"linux",|handler| {let output=json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":handler.config.command}}).to_string();run(handler,output)}).await?;assert!(matches!(result.decision,HookDispatchDecision::Ask {..}));Ok(())}
    #[tokio::test] async fn last_allow_replaces_input()->std::io::Result<()> {let handlers=vec![handler("first",0),handler("second",1)];let result=dispatch_hook_event(&handlers,&json!({"event":"PreToolUse"}),&state(&handlers)?,"linux",|handler| {let output=json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"allow","updatedInput":{"command":handler.config.command}}}).to_string();run(handler,output)}).await?;assert!(matches!(result.decision,HookDispatchDecision::Allow {updated_input:Some(ref value),..} if value==&json!({"command":"second"})));Ok(())}
    #[tokio::test] async fn skips_untrusted_but_lists_matched()->std::io::Result<()> {let handlers=vec![handler("trusted",0),handler("untrusted",1)];let result=dispatch_hook_event(&handlers,&json!({"event":"PreToolUse"}),&state(&handlers[..1])?,"linux",|handler|run(handler,String::new())).await?;assert_eq!(result.matched_handlers.len(),2);assert_eq!(result.summaries.len(),1);assert_eq!(result.skipped[0].reason,"untrusted");Ok(())}
    #[tokio::test] async fn malformed_output_nonfatal()->std::io::Result<()> {let handlers=vec![handler("malformed",0)];let result=dispatch_hook_event(&handlers,&json!({"event":"PreToolUse"}),&state(&handlers)?,"linux",|handler|run(handler,"{not json".to_owned())).await?;assert_eq!(result.decision,HookDispatchDecision::None);assert_eq!(result.diagnostics[0].code,"invalid_root");Ok(())}
}
