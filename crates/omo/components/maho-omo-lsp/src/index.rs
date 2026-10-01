use std::{path::PathBuf,sync::{Arc,Mutex}};
use maho_ext_api::{EventKind,EventResult,Extension,ExtensionApi,ExtensionEvent,ExtensionFailure,ExtensionWidgetOptions,FlagType,FlagValue,ToolContent,ToolError,ToolResult,ToolResultEventResult,WidgetPlacement};
use lsp_core::post_edit::{DiagnosticsRunnerError,PostEditDiagnosticsOutcome};
use crate::{adapter::descriptors::descriptors,daemon_tool_client::{call_packaged_daemon_tool,current_senpi_request_context},post_edit_diagnostics::{append_post_edit_diagnostics,LspPostEditSessionState,POST_EDIT_DIAGNOSTICS_WIDGET_KEY,should_run_post_edit_diagnostics}};
const TOOLS_FLAG:&str="omo-senpi-lsp-tools-enabled";
const POST_EDIT_FLAG:&str="omo-senpi-lsp-post-edit-diagnostics-enabled";
#[derive(Default)]
pub struct LspComponent;
impl Extension for LspComponent {
    fn register(&self,api:&mut ExtensionApi) {
        api.register_flag(TOOLS_FLAG,FlagType::Boolean {default:Some(true)},Some("Enable omo-senpi LSP tools.".into()));
        api.register_flag(POST_EDIT_FLAG,FlagType::Boolean {default:Some(true)},Some("Enable omo-senpi post-edit LSP diagnostics.".into()));
        if api.get_flag(TOOLS_FLAG)==Some(FlagValue::Boolean(false)) {return;}
        let home=PathBuf::from(std::env::var("HOME").unwrap_or_default());
        for mut tool in descriptors() {
            let name=tool.name.clone();let cwd=api.cwd.clone();let home=home.clone();
            tool.execute=Arc::new(move |call| {let name=name.clone();let cwd=cwd.clone();let home=home.clone();Box::pin(async move {
                let context=current_senpi_request_context(&cwd,&home)?;
                let args=call.params.as_object().cloned().unwrap_or_default();
                let result=call_packaged_daemon_tool(&name,args,context,call.signal).await?;
                if result.is_error {return Err(ToolError::Message(result.text().into()));}
                let content=result.content.into_iter().map(serde_json::from_value::<ToolContent>).collect::<Result<Vec<_>,_>>()?;
                Ok(ToolResult {content,details:result.details})
            })});
            api.register_tool(tool);
        }
        let state=Arc::new(Mutex::new(LspPostEditSessionState::default()));
        if api.get_flag(POST_EDIT_FLAG)!=Some(FlagValue::Boolean(false)) {
            let caches=Arc::clone(&state);let home=home.clone();
            api.on(EventKind::ToolResult,Arc::new(move |event,ctx| {let caches=Arc::clone(&caches);let home=home.clone();Box::pin(async move {
                let ExtensionEvent::ToolResult(event)=event else {return Ok(EventResult::None);};
                if should_run_post_edit_diagnostics(event) && let Some(update)=&ctx.update_tool_hook_status {update("(OmO) Checking LSP Diagnostics");}
                let cache=caches.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_or_create(Some(ctx.session_manager.session_id()));
                let context=current_senpi_request_context(&ctx.cwd,&home).map_err(|e|ExtensionFailure::new(e.to_string()))?;
                let signal=ctx.signal.clone().unwrap_or_default();
                let result=append_post_edit_diagnostics(event,move |path| {let context=context.clone();let signal=signal.clone();async move {
                    let args=serde_json::json!({"filePath":path,"severity":"error"}).as_object().cloned().unwrap_or_default();
                    let result=call_packaged_daemon_tool("lsp_diagnostics",args,context,signal).await.map_err(|e|DiagnosticsRunnerError::new(e.to_string()))?;
                    if let Some(availability)=result.details.as_ref().and_then(|v|v.get("availability")) && availability.get("kind").and_then(serde_json::Value::as_str)==Some("not_configured") && let Some(extension)=availability.get("extension").and_then(serde_json::Value::as_str).filter(|s|!s.is_empty()) {return Ok(PostEditDiagnosticsOutcome::NotConfigured {extension:extension.into()});}
                    Ok(PostEditDiagnosticsOutcome::Text(result.content.iter().filter(|b|b.get("type").and_then(serde_json::Value::as_str)==Some("text")).filter_map(|b|b.get("text").and_then(serde_json::Value::as_str)).collect::<Vec<_>>().join("\n")))
                }},Some(&cache)).await;
                if let Some(result)=result {ctx.ui.set_widget(POST_EDIT_DIAGNOSTICS_WIDGET_KEY,None,ExtensionWidgetOptions {placement:WidgetPlacement::BelowEditor});if let Some(content)=result.content {return Ok(EventResult::ToolResult(ToolResultEventResult {content:Some(content),details:None,is_error:None,usage:None}));}}
                Ok(EventResult::None)
            })}));
            for kind in [EventKind::SessionStart,EventKind::SessionCompact] {let caches=Arc::clone(&state);api.on(kind,Arc::new(move |_,ctx| {let caches=Arc::clone(&caches);Box::pin(async move {let mut caches=caches.lock().unwrap_or_else(std::sync::PoisonError::into_inner);let id=Some(ctx.session_manager.session_id());if kind==EventKind::SessionStart {caches.on_session_start(id);} else {caches.reset(id);}Ok(EventResult::None)})}));}
        }
        api.on(EventKind::SessionShutdown,Arc::new(move |_,ctx| {let state=Arc::clone(&state);Box::pin(async move {state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).delete(Some(ctx.session_manager.session_id()));Ok(EventResult::None)})}));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{EventBus,ExtensionRuntime,ExtensionSessionProfile,LoadedExtension,SourceInfo};
    fn api()->ExtensionApi {ExtensionApi::new(LoadedExtension::new("lsp",PathBuf::from("/workspace"),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default())}
    #[test] fn descriptor_fields_preserved() {let mut a=api();LspComponent.register(&mut a);let expected=descriptors();for (tool,legacy) in a.registered.tools.iter().zip(expected) {assert_eq!(tool.definition.parameters,legacy.parameters);assert_eq!(tool.definition.label,legacy.label);assert_eq!(tool.definition.execution_mode,legacy.execution_mode);assert!(!Arc::ptr_eq(&tool.definition.execute,&legacy.execute));}}
    #[test] fn exact_six_tools() {let mut a=api();LspComponent.register(&mut a);assert_eq!(a.registered.tools.len(),6);}
    #[test] fn disabled_tools_register_flags_only() {let mut a=api();a.runtime.set_flag(TOOLS_FLAG,FlagValue::Boolean(false));LspComponent.register(&mut a);assert!(a.registered.tools.is_empty());assert_eq!(a.registered.flags.len(),2);}
    #[test] fn missing_server_still_registers() {let mut a=api();LspComponent.register(&mut a);assert_eq!(a.registered.tools.len(),6);for kind in [EventKind::SessionStart,EventKind::SessionShutdown,EventKind::SessionCompact,EventKind::ToolResult] {assert!(a.registered.handlers.contains_key(&kind));}}
}
