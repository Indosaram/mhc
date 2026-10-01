use std::collections::{BTreeMap,VecDeque};
use std::sync::{Arc,Mutex};
use maho_ext_api::types::*;
use crate::config_loader::{HookConfigLoaderOptions,load_hook_config_files};
use crate::trust_storage::FileHookStateStorage;
use crate::trust::HookTrustStorageScope;
use crate::trust_state_json::empty_hook_trust_state;
use crate::types::{ParsedHookConfig,HookTrustState};
use crate::dispatcher::{dispatch_hook_event,HookDispatchDecision,HookDispatchResult};
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
    let state=refresh_state(ctx)?;let cwd=ctx.cwd.clone();let signal=ctx.signal.clone();let wire=input.clone();
    dispatch_hook_event(&state.parsed.executable_handlers,&input,&state.trust,if cfg!(windows) {"win32"} else {"linux"},|handler| {let cwd=cwd.clone();let signal=signal.clone();let wire=wire.clone();async move {run_command_hook(&handler,&wire,CommandHookRunOptions {cwd:&cwd,env_passthrough:&[],output_policy:None,signal:signal.as_ref(),source_env:None}).await}}).await.map_err(|error|ExtensionFailure::new(error.to_string()))
}
#[derive(Default)]
struct Pending {prompts:VecDeque<PendingPromptHookContext>,pre_tools:BTreeMap<String,Vec<String>>}
pub struct HooksExtension;
impl Extension for HooksExtension {
    fn register(&self,api:&mut ExtensionApi) {
        let pending=Arc::new(Mutex::new(Pending::default()));
        for kind in [EventKind::Input,EventKind::BeforeAgentStart,EventKind::ToolCall,EventKind::ToolResult] {
            let pending=Arc::clone(&pending);
            api.on(kind,Arc::new(move |event,ctx| {let pending=Arc::clone(&pending);Box::pin(async move {
                match event {
                    ExtensionEvent::Input(event) if event.source!=InputSource::Extension=> {
                        pending.lock().map_err(|_|ExtensionFailure::new("hooks pending state poisoned"))?.prompts.clear();
                        let transcript=ctx.session_manager.session_file().map(|path|path.to_string_lossy().into_owned());
                        let result=dispatch(ctx,build_user_prompt_hook_input(UserPromptHookInputOptions {cwd:&ctx.cwd.to_string_lossy(),permission_mode:"default",prompt:&event.text,session_id:ctx.session_manager.session_id(),transcript_path:transcript.as_deref()})).await?;
                        if matches!(result.decision,HookDispatchDecision::Block {..}) {ctx.ui.notify(&prompt_block_reason_from_result(&result),NotificationType::Warning);return Ok(EventResult::Input(InputEventResult::Handled));}
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
        crate::command::register_hooks_command(api);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn registers_native_events_and_command_context() {let mut api=ExtensionApi::new(LoadedExtension::new("hooks","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());HooksExtension.register(&mut api);for event in [EventKind::Input,EventKind::BeforeAgentStart,EventKind::ToolCall,EventKind::ToolResult] {assert_eq!(api.registered.handlers[&event].len(),1);}assert_eq!(api.registered.commands[0].name,"hooks");assert!(api.registered.command_context_handlers.contains_key("hooks"));}
}
