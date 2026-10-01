use std::sync::{Arc,Mutex};
use maho_ext_api::types::{Extension,ExtensionApi,EventKind,EventResult,ExtensionFailure};
use maho_tools::definition::{ToolDefinition,ToolResult,ToolContent,ToolError};
use serde_json::{Value,json};
use crate::manager::TerminalManager;
use crate::tools::{bash_input::{execute_bash_input,BashInputInput},bash_output::execute_bash_output,bash_resize::execute_bash_resize,kill_bash::execute_kill_bash,context::TerminalToolResult};
pub struct TerminalExtension;
fn tool_result(result:TerminalToolResult)->Result<ToolResult,ToolError> {
    if result.is_error==Some(true) {return Err(ToolError::Message(result.content.into_iter().map(|part|part.text).collect::<Vec<_>>().join("\n")));}
    Ok(ToolResult {content:result.content.into_iter().map(|part|ToolContent::Text {text:part.text,audience:part.audience.map(|_|"model".to_owned())}).collect(),details:result.details.map(Value::Object)})
}
impl Extension for TerminalExtension {
    fn register(&self,api:&mut ExtensionApi) {
        let manager=Arc::new(Mutex::new(TerminalManager::default()));
        let notifier:Arc<Mutex<Option<crate::monitor_notify::MonitorNotifier>>>=Arc::new(Mutex::new(None));
        let event_notifier=notifier.clone();
        let monitors=Arc::new(Mutex::new(crate::monitor_registry::MonitorRegistry::new(move |event| {
            if let Some(notifier)=event_notifier.lock().expect("monitor notifier").as_ref() && let Err(error)=notifier.notify_event(event) {eprintln!("monitor delivery failed: {error}");}
        })));
        let sender=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));
        let lifecycle_notifier=notifier.clone();let lifecycle_monitors=monitors.clone();
        api.on(EventKind::SessionStart,Arc::new(move |_,ctx| {let notifier=lifecycle_notifier.clone();let monitors=lifecycle_monitors.clone();let sender=sender.clone();Box::pin(async move {
            use maho_ext_api::types::{ExtensionMode,CustomMessage,SendMessageOptions,DeliverAs};
            if matches!(ctx.mode,ExtensionMode::Print|ExtensionMode::Json)||ctx.model.is_none() {return Ok(EventResult::None);}
            let delivery=crate::monitor_notify::MonitorNotifier::new(crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor,move |injection| {
                if !injection.pause_ids.is_empty() {monitors.lock().expect("monitor registry").pause(&injection.pause_ids);}
                if let Err(error)=sender.send_message(CustomMessage {custom_type:crate::monitor_notify::MONITOR_NOTIFICATION_CUSTOM_TYPE.to_owned(),content:vec![ToolContent::text(injection.content)],display:false,details:Some(injection.details)},SendMessageOptions {trigger_turn:true,deliver_as:Some(DeliverAs::Steer)}) {eprintln!("monitor notification failed: {error}");}
            });
            *notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?=Some(delivery);Ok(EventResult::None)
        })}));
        let bash_manager=Arc::clone(&manager);
        let mut bash=ToolDefinition::new("bash","Execute a shell command in a persistent PTY-backed session.",json!({"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"number"},"description":{"type":"string"},"run_in_background":{"type":"boolean"},"cols":{"type":"number"},"rows":{"type":"number"}},"required":["command"]}),Arc::new(move |call| {let manager=Arc::clone(&bash_manager);Box::pin(async move {tool_result(crate::tools::bash::execute_bash(manager,call).await.map_err(ToolError::Message)?)})}));
        bash.exposure=Some(maho_tools::definition::ToolExposure::Eval);api.register_tool(bash);
        for (name,description,properties,required) in [
            ("bash_output","Read output from a background bash session without blocking.",json!({"bash_id":{"type":"string"},"filter":{"type":"string"},"view":{"type":"string","enum":["log","screen"]}}),vec!["bash_id"]),
            ("bash_input","Write stdin or named keys to a live background bash session.",json!({"bash_id":{"type":"string"},"input":{"type":"string"},"keys":{"type":"array","items":{"type":"string"}},"submit":{"type":"boolean"}}),vec!["bash_id"]),
            ("bash_resize","Resize a live background bash session's PTY.",json!({"bash_id":{"type":"string"},"cols":{"type":"number"},"rows":{"type":"number"}}),vec!["bash_id","cols","rows"]),
            ("kill_bash","Terminate a background bash session and its process tree.",json!({"bash_id":{"type":"string"},"all":{"type":"boolean"}}),Vec::new()),
        ] {
            let manager=Arc::clone(&manager);
            let monitors=monitors.clone();
            api.register_tool(ToolDefinition::new(name,description,json!({"type":"object","properties":properties,"required":required}),Arc::new(move |call| {let manager=Arc::clone(&manager);let monitors=monitors.clone();Box::pin(async move {
                let mut manager=manager.lock().map_err(|_|ToolError::Message("terminal manager state poisoned".to_owned()))?;
                let id=call.params.get("bash_id").and_then(Value::as_str);
                if name=="kill_bash" {
                    let mut monitors=monitors.lock().map_err(|_|ToolError::Message("monitor registry state poisoned".to_owned()))?;
                    let all=call.params.get("all").and_then(Value::as_bool).unwrap_or(false);
                    if all {
                        let mut count=manager.size();
                        for id in monitors.snapshot().into_iter().filter(|record|record.id.starts_with("watch_")).map(|record|record.id) {count+=usize::from(monitors.stop_file(&id));}
                        manager.teardown().map_err(|error|ToolError::Message(error.to_string()))?;
                        return tool_result(crate::tools::context::text_result(format!("Killed {count} session(s).")));
                    }
                    if let Some(id)=id {
                        let resolved=manager.resolve_id(id).unwrap_or_else(||id.to_owned());
                        if monitors.stop_file(&resolved) {return tool_result(crate::tools::context::text_result(format!("Killed {id}.")));}
                    }
                    return tool_result(execute_kill_bash(&mut manager,id,all));
                }
                let id=id.ok_or_else(||ToolError::Message("bash_id must be a string".to_owned()))?;let resolved=manager.resolve_id(id).unwrap_or_else(||id.to_owned());let runtime=manager.get(&resolved);
                match name {
                    "bash_output"=>{if call.params.get("view").and_then(Value::as_str)==Some("screen") {return Err(ToolError::Message("Native terminal screen projection is not ported.".to_owned()));}tool_result(execute_bash_output(runtime.as_deref(),id,call.params.get("filter").and_then(Value::as_str)).map_err(|error|ToolError::Message(error.to_string()))?)},
                    "bash_input"=>{let keys=call.params.get("keys").and_then(Value::as_array).map(|keys|keys.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>()).unwrap_or_default();tool_result(execute_bash_input(runtime,BashInputInput {bash_id:id,input:call.params.get("input").and_then(Value::as_str),keys:&keys,submit:call.params.get("submit").and_then(Value::as_bool)}))},
                    "bash_resize"=>tool_result(execute_bash_resize(runtime.as_deref(),id,call.params.get("cols").and_then(Value::as_f64).unwrap_or(f64::NAN),call.params.get("rows").and_then(Value::as_f64).unwrap_or(f64::NAN))),_=>unreachable!(),
                }
            })})));
        }
        let tool_manager=manager.clone();let tool_monitors=monitors.clone();let tool_notifier=notifier.clone();
        api.register_tool(ToolDefinition::new("monitor","Subscribe to command output or file changes instead of polling.",crate::tools::monitor::monitor_schema(),Arc::new(move |call| {
            let manager=tool_manager.clone();let monitors=tool_monitors.clone();let notifier=tool_notifier.clone();Box::pin(async move {
                let mut manager=manager.lock().map_err(|_|ToolError::Message("terminal manager state poisoned".to_owned()))?;
                let mut monitors=monitors.lock().map_err(|_|ToolError::Message("monitor registry state poisoned".to_owned()))?;
                let cwd=call.context.map(|context|context.cwd().to_path_buf()).unwrap_or(std::env::current_dir()?);
                let result=crate::tools::monitor::execute_monitor(&mut manager,&mut monitors,&call.params,&cwd);
                if call.params.get("action").and_then(Value::as_str)==Some("rearm") && let Some(notifier)=notifier.lock().map_err(|_|ToolError::Message("monitor notifier state poisoned".to_owned()))?.as_ref() {
                    notifier.resume(monitors.snapshot().iter().filter(|record|!record.paused).map(|record|record.id.clone()).collect()).map_err(ToolError::Message)?;
                }
                tool_result(result)
            })
        })));
        let activity=notifier.clone();
        let input_monitors=monitors.clone();
        api.on(EventKind::Input,Arc::new(move |event,_| {let notifier=activity.clone();let monitors=input_monitors.clone();Box::pin(async move {
            if matches!(event,maho_ext_api::types::ExtensionEvent::Input(event) if event.source==maho_ext_api::types::InputSource::Extension) {return Ok(EventResult::None);}
            let resumed=monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?.resume(None);
            if let Some(notifier)=notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.as_ref() {notifier.note_activity().map_err(ExtensionFailure::new)?;if !resumed.is_empty() {notifier.resume(resumed.into_iter().map(|(id,_)|id).collect()).map_err(ExtensionFailure::new)?;}}
            Ok(EventResult::None)
        })}));
        let activity=notifier.clone();
        api.on(EventKind::ToolCall,Arc::new(move |_,_| {let notifier=activity.clone();Box::pin(async move {if let Some(notifier)=notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.as_ref() {notifier.note_activity().map_err(ExtensionFailure::new)?;}Ok(EventResult::None)})}));
        let cleanup=Arc::clone(&manager);
        api.on(EventKind::SessionShutdown,Arc::new(move |_,_| {let manager=Arc::clone(&cleanup);let monitors=monitors.clone();let notifier=notifier.clone();Box::pin(async move {notifier.lock().map_err(|_|ExtensionFailure::new("monitor notifier state poisoned"))?.take();monitors.lock().map_err(|_|ExtensionFailure::new("monitor registry state poisoned"))?.dispose();manager.lock().map_err(|_|ExtensionFailure::new("terminal manager state poisoned"))?.teardown().map_err(|error|ExtensionFailure::new(error.to_string()))?;Ok(EventResult::None)})}));
    }
}
#[cfg(test)]
mod tests {
    use super::*;use maho_ext_api::types::*;
    #[tokio::test] async fn registered_bash_runs_real_pty_and_companion_peeks()->Result<(),ToolError> {
        let mut api=ExtensionApi::new(LoadedExtension::new("terminal","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());TerminalExtension.register(&mut api);
        let result=(api.registered.tools[0].definition.execute)(maho_tools::definition::ToolCall {id:"c1",params:json!({"command":"stty -echo; printf 'ready\\n'"}),signal:Default::default(),on_update:None,context:None}).await?;
        assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text=="ready"));
        let id="bash_1";
        let result=(api.registered.tools[1].definition.execute)(maho_tools::definition::ToolCall {id:"c2",params:json!({"bash_id":id}),signal:Default::default(),on_update:None,context:None}).await?;
        assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text.contains("status: completed exit_code: 0")));
        (api.registered.tools[4].definition.execute)(maho_tools::definition::ToolCall {id:"c3",params:json!({"all":true}),signal:Default::default(),on_update:None,context:None}).await?;Ok(())
    }
    #[test] fn native_companions_register_flat_schemas_and_shutdown() {let mut api=ExtensionApi::new(LoadedExtension::new("terminal","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());TerminalExtension.register(&mut api);assert_eq!(api.registered.tools.iter().map(|tool|tool.definition.name.as_str()).collect::<Vec<_>>(),vec!["bash","bash_output","bash_input","bash_resize","kill_bash","monitor"]);for tool in &api.registered.tools {assert_eq!(tool.definition.parameters["type"],"object");assert!(tool.definition.parameters.get("properties").is_some());}assert_eq!(api.registered.handlers[&EventKind::SessionShutdown].len(),1);}
}
