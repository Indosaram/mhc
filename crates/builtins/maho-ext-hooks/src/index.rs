use std::collections::{BTreeMap,VecDeque};
use std::sync::{Arc,Mutex};
use maho_ext_api::types::*;
use crate::config_loader::{HookConfigLoaderOptions,load_hook_config_files};
use crate::trust_storage::FileHookStateStorage;
use crate::trust::HookTrustStorageScope;
use crate::trust_state_json::empty_hook_trust_state;
use crate::types::{ParsedHookConfig,HookTrustState};
use crate::dispatcher::{dispatch_hook_event_with_status,HookDispatchDecision,HookDispatchResult};
use crate::command_runner::{run_command_hook,CommandHookRunOptions};
use crate::tool_adapter::*;
use crate::prompt_adapter::*;

pub struct HookRuntimeState {pub parsed:ParsedHookConfig,pub trust:HookTrustState,pub storage:FileHookStateStorage}
pub fn refresh_state(ctx:&ExtensionContext)->Result<HookRuntimeState,ExtensionFailure> {
    let sources=ctx.get_loaded_hook_sources()?;
    let text=|path:&std::path::Path|path.to_string_lossy().into_owned();
    let paths=|paths:&[std::path::PathBuf]|paths.iter().map(|path|text(path)).collect();
    let options=HookConfigLoaderOptions {cwd:text(&sources.cwd),agent_dir:text(&sources.agent_dir),global_settings_hooks:sources.global_settings_hooks,project_settings_hooks:sources.project_settings_hooks,global_hooks_path:Some(text(&sources.global_hooks_path)),project_hooks_path:Some(text(&sources.project_hooks_path)),global_hook_source_paths:paths(&sources.global_hook_source_paths),project_hook_source_paths:paths(&sources.project_hook_source_paths),pre_session_hook_source_paths:paths(&sources.pre_session_hook_source_paths),runtime_hook_source_paths:paths(&sources.runtime_hook_source_paths)};
    let parsed=load_hook_config_files(&options);let storage=FileHookStateStorage::new(&sources.agent_dir,&sources.cwd);
    let mut trust=storage.read(HookTrustStorageScope::Global).map_err(|error|ExtensionFailure::new(error.to_string()))?;
    let project=if ctx.is_project_trusted() {storage.read(HookTrustStorageScope::Project).map_err(|error|ExtensionFailure::new(error.to_string()))?} else {empty_hook_trust_state()};trust.hooks.extend(project.hooks);
    Ok(HookRuntimeState {parsed,trust,storage})
}
async fn dispatch(ctx:&ExtensionContext,input:serde_json::Value)->Result<HookDispatchResult,ExtensionFailure> {
    let mut state=refresh_state(ctx)?;let cwd=ctx.cwd.clone();let signal=ctx.signal.clone();let wire=input.clone();
    if matches!(input.get("event").and_then(serde_json::Value::as_str),Some("SessionStart"|"PreCompact"|"PostCompact"|"Notification")) {
        let platform=if cfg!(windows) {"win32"} else {"linux"};let mut selected=Vec::new();let mut trust=empty_hook_trust_state();
        for handler in state.parsed.executable_handlers {
            if Some(handler.event.as_str())!=input.get("event").and_then(serde_json::Value::as_str) {continue;}
            if handler.event==crate::types::SupportedHookEvent::SessionStart&&input.get("reason").and_then(serde_json::Value::as_str)==Some("startup")&&handler.source.discovered_at==crate::types::HookDiscoveryTiming::Runtime {continue;}
            let matcher=handler.matcher.as_deref().unwrap_or("").trim();
            let subjects=[handler.event.as_str(),input.get("reason").and_then(serde_json::Value::as_str).unwrap_or("")];
            let matched=handler.event==crate::types::SupportedHookEvent::Notification||matcher.is_empty()||matcher=="*"||matcher.split(['|',',']).map(str::trim).any(|part|subjects.contains(&part))||fancy_regex::Regex::new(matcher).is_ok_and(|regex|subjects.iter().any(|subject|regex.is_match(subject).unwrap_or(false)));
            if !matched||!crate::trust::is_command_hook_trusted(&handler,&state.trust,platform).map_err(|error|ExtensionFailure::new(error.to_string()))? {continue;}
            let mut handler=handler;handler.matcher=None;
            trust.hooks.insert(crate::trust::hook_trust_id(&handler),crate::trust::create_hook_trust_entry(&handler,platform,"2026-06-29T00:00:00.000Z").map_err(|error|ExtensionFailure::new(error.to_string()))?);selected.push(handler);
        }
        state.parsed.executable_handlers=selected;state.trust=trust;
    }
    let platform=if cfg!(windows) {"win32"} else {"linux"};
    let tool_status=matches!(input.get("event").and_then(serde_json::Value::as_str),Some("PreToolUse"|"PostToolUse"));
    dispatch_hook_event_with_status(&state.parsed.executable_handlers,&input,&state.trust,platform,|handler| {let cwd=cwd.clone();let signal=signal.clone();let wire=wire.clone();async move {run_command_hook(&handler,&wire,CommandHookRunOptions {cwd:&cwd,env_passthrough:&[],output_policy:None,signal:signal.as_ref(),source_env:None}).await}},|running| {if tool_status && !running.is_empty() && let Some(update)=&ctx.update_tool_hook_status {update(&crate::dispatcher::running_hook_handlers_status_label(running,platform));}}).await.map_err(|error|ExtensionFailure::new(error.to_string()))
}
#[derive(Default)]
struct Pending {prompts:VecDeque<PendingPromptHookContext>,pre_tools:BTreeMap<String,Vec<String>>,stop_tracker:crate::stop_adapter::StopTurnTracker}
pub struct HooksExtension;
impl Extension for HooksExtension {
    fn register(&self,api:&mut ExtensionApi) {
        let pending=Arc::new(Mutex::new(Pending::default()));
        let sender=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));
        let wake_sources=Arc::new(Mutex::new(BTreeMap::<String,f64>::new()));
        let sources=wake_sources.clone();
        let subscription=Arc::new(api.events.on("wake_source_state",Arc::new(move |data| {
            let Some(source)=data.get("source").and_then(serde_json::Value::as_str) else {return;};
            let Some(count)=data.get("activeCount").and_then(serde_json::Value::as_f64).filter(|count|count.is_finite()&&*count>=0.0) else {return;};
            if source=="ask-user" {return;}
            let mut sources=sources.lock().expect("hooks wake sources");if count>0.0 {sources.insert(source.to_owned(),count);} else {sources.remove(source);}
        })));
        let notification_sender=sender.clone();
        api.on(EventKind::AgentSettled,Arc::new(move |_,ctx| {let sources=wake_sources.clone();let sender=notification_sender.clone();let subscription=subscription.clone();Box::pin(async move {
            let _subscription=subscription;
            let summary=sources.lock().map_err(|_|ExtensionFailure::new("hooks wake sources poisoned"))?.iter().map(|(source,count)|format!("{source} ({count})")).collect::<Vec<_>>().join(", ");
            if summary.is_empty() {return Ok(EventResult::None);}
            use crate::lifecycle_adapter::*;
            let cwd=ctx.cwd.to_string_lossy();let transcript=ctx.session_manager.session_file().map(|path|path.to_string_lossy().into_owned());
            let input=build_notification_hook_input(NotificationHookInput {kind:"turn-settled",message:&format!("Turn settled while background work is still active: {summary}."),source:Some("wake-source"),title:Some("Background work still active"),request_id:None,status:None},&LifecycleInputContext {cwd:&cwd,session_id:ctx.session_manager.session_id(),transcript_path:transcript.as_deref()});
            let result=dispatch(ctx,input).await?;let details=lifecycle_result_details("Notification",Some(&result));
            if let Some(message)=lifecycle_message("Notification",&details,None) {sender.send_message(message,SendMessageOptions::default())?;}
            Ok(EventResult::None)
        })}));
        for kind in [EventKind::SessionStart,EventKind::SessionBeforeCompact,EventKind::SessionCompact] {
            let sender=Arc::clone(&sender);
            api.on(kind,Arc::new(move |event,ctx| {let sender=Arc::clone(&sender);Box::pin(async move {
                use crate::lifecycle_adapter::*;
                let cwd=ctx.cwd.to_string_lossy();let transcript=ctx.session_manager.session_file().map(|path|path.to_string_lossy().into_owned());
                let context=LifecycleInputContext {cwd:&cwd,session_id:ctx.session_manager.session_id(),transcript_path:transcript.as_deref()};
                let session_reason=|reason:SessionReason|match reason {SessionReason::Startup=>"startup",SessionReason::Reload=>"reload",SessionReason::New=>"new",SessionReason::Resume=>"resume",SessionReason::Fork=>"fork",SessionReason::Quit=>"quit"};
                let compact_reason=|reason:CompactionReason|match reason {CompactionReason::Manual=>"manual",CompactionReason::Threshold=>"threshold",CompactionReason::Overflow=>"overflow",CompactionReason::PrePrompt=>"pre-prompt",CompactionReason::Branch=>"branch",CompactionReason::Extension=>"extension"};
                let (name,input,request_id)=match event {
                    ExtensionEvent::SessionStart(event)=>("SessionStart",build_session_start_hook_input(session_reason(event.reason),&context),None),
                    ExtensionEvent::SessionBeforeCompact(event)=>("PreCompact",build_pre_compact_hook_input(compact_reason(event.reason),&event.request_id,event.will_retry,event.custom_instructions.as_deref(),&context),Some(event.request_id.clone())),
                    ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted {reason,request_id,will_retry,..})=>("PostCompact",build_post_compact_hook_input(compact_reason(*reason),request_id,*will_retry,true,&context),Some(request_id.clone())),
                    _=>return Ok(EventResult::None),
                };
                let result=dispatch(ctx,input).await?;let details=lifecycle_result_details(name,Some(&result));
                if let Some(message)=lifecycle_message(name,&details,request_id.as_deref()) {sender.send_message(message,SendMessageOptions::default())?;}
                if name=="PreCompact"&&details.cancel {Ok(EventResult::SessionBefore(SessionBeforeEventResult {cancel:Some(true),..Default::default()}))} else {Ok(EventResult::None)}
            })}));
        }
        for kind in [EventKind::Input,EventKind::BeforeAgentStart,EventKind::ToolCall,EventKind::ToolResult] {
            let pending=Arc::clone(&pending);
            let sender=Arc::clone(&sender);
            api.on(kind,Arc::new(move |event,ctx| {let pending=Arc::clone(&pending);let sender=Arc::clone(&sender);Box::pin(async move {
                match event {
                    ExtensionEvent::Input(event) if event.source!=InputSource::Extension=> {
                        pending.lock().map_err(|_|ExtensionFailure::new("hooks pending state poisoned"))?.prompts.clear();
                        pending.lock().map_err(|_|ExtensionFailure::new("hooks pending state poisoned"))?.stop_tracker.reset();
                        let transcript=ctx.session_manager.session_file().map(|path|path.to_string_lossy().into_owned());
                        let result=dispatch(ctx,build_user_prompt_hook_input(UserPromptHookInputOptions {cwd:&ctx.cwd.to_string_lossy(),permission_mode:"default",prompt:&event.text,session_id:ctx.session_manager.session_id(),transcript_path:transcript.as_deref()})).await?;
                        if let HookDispatchDecision::Block {source,..}=&result.decision {let reason=prompt_block_reason_from_result(&result);ctx.ui.notify(&reason,NotificationType::Warning);sender.send_message(CustomMessage {custom_type:HOOK_CUSTOM_MESSAGE_TYPE.to_owned(),content:vec![ToolContent::text(reason)],display:false,details:Some(serde_json::json!({"decision":"block","event":"UserPromptSubmit","sourcePath":source.source_path}))},SendMessageOptions::default())?;return Ok(EventResult::Input(InputEventResult::Handled));}
                        if ctx.is_idle() && let Some(context)=prompt_context_from_result(&result) {pending.lock().map_err(|_|ExtensionFailure::new("hooks pending state poisoned"))?.prompts.push_back(context);}
                        Ok(EventResult::Input(InputEventResult::Continue))
                    },
                    ExtensionEvent::BeforeAgentStart(event)=> {
                        let Some(context)=pending.lock().map_err(|_|ExtensionFailure::new("hooks pending state poisoned"))?.prompts.pop_front() else {return Ok(EventResult::None);};
                        let message=format_prompt_context_message(&context).map(|text|CustomMessage {custom_type:HOOK_CUSTOM_MESSAGE_TYPE.to_owned(),content:vec![ToolContent::text(text)],display:false,details:Some(serde_json::json!({"event":"UserPromptSubmit","diagnostics":context.diagnostics.iter().map(safe_diagnostic_details).collect::<Vec<_>>()}))});
                        let system_prompt=append_system_messages(&event.system_prompt,&context.system_messages);Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {message,system_prompt:if system_prompt!=event.system_prompt {Some(system_prompt)} else {None}}))
                    },
                    ExtensionEvent::ToolCall(event)=> {
                        pending.lock().map_err(|_|ExtensionFailure::new("hooks pending state poisoned"))?.pre_tools.remove(&event.tool_call_id);
                        let result=dispatch(ctx,build_pre_tool_use_hook_input(event,&ctx.cwd.to_string_lossy(),ctx.session_manager.session_id())).await?;
                        let applied=apply_pre_tool_use_result(event,&result);
                        if applied.as_ref().is_none_or(|result|result.block!=Some(true)) {let contexts=tool_contexts_from_result(&result);if !contexts.is_empty() {pending.lock().map_err(|_|ExtensionFailure::new("hooks pending state poisoned"))?.pre_tools.insert(event.tool_call_id.clone(),contexts);}}
                        Ok(applied.map(EventResult::ToolCall).unwrap_or(EventResult::None))
                    },
                    ExtensionEvent::ToolResult(event)=> {let contexts=pending.lock().map_err(|_|ExtensionFailure::new("hooks pending state poisoned"))?.pre_tools.remove(&event.tool_call_id).unwrap_or_default();let result=dispatch(ctx,build_post_tool_use_hook_input(event,&ctx.cwd.to_string_lossy(),ctx.session_manager.session_id())).await?;Ok(apply_post_tool_use_result(event,&result,&contexts).map(EventResult::ToolResult).unwrap_or(EventResult::None))},
                    _=>Ok(EventResult::None),
                }
            })}));
        }
        let stop_pending=Arc::clone(&pending);let stop_sender=Arc::clone(&sender);
        api.on(EventKind::AgentEnd,Arc::new(move |event,ctx| {let pending=Arc::clone(&stop_pending);let sender=Arc::clone(&stop_sender);Box::pin(async move {
            let ExtensionEvent::AgentEnd {messages,..}=event else {return Ok(EventResult::None);};
            let messages=messages.iter().map(serde_json::to_value).collect::<Result<Vec<_>,_>>().map_err(|error|ExtensionFailure::new(error.to_string()))?;
            let transcript=ctx.session_manager.session_file().map(|path|path.to_string_lossy().into_owned());let input=crate::stop_adapter::build_stop_hook_input(&messages,&ctx.cwd.to_string_lossy(),ctx.session_manager.session_id(),transcript.as_deref());
            let result=dispatch(ctx,input).await?;let leaf=ctx.session_manager.get_leaf_id();let turn_key=pending.lock().map_err(|_|ExtensionFailure::new("hooks pending state poisoned"))?.stop_tracker.turn_key(leaf.as_deref(),ctx.session_manager.session_id());
            crate::stop_adapter::apply_stop_hook_result(&sender,ctx,&result,&turn_key).await?;Ok(EventResult::None)
        })}));
        crate::command::register_hooks_command(api);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn registers_native_events_and_command_context() {let mut api=ExtensionApi::new(LoadedExtension::new("hooks","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());HooksExtension.register(&mut api);for event in [EventKind::Input,EventKind::BeforeAgentStart,EventKind::ToolCall,EventKind::ToolResult] {assert_eq!(api.registered.handlers[&event].len(),1);}assert_eq!(api.registered.commands[0].name,"hooks");assert!(api.registered.command_context_handlers.contains_key("hooks"));}
}
