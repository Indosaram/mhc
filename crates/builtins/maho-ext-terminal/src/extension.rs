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
            api.register_tool(ToolDefinition::new(name,description,json!({"type":"object","properties":properties,"required":required}),Arc::new(move |call| {let manager=Arc::clone(&manager);Box::pin(async move {
                let mut manager=manager.lock().map_err(|_|ToolError::Message("terminal manager state poisoned".to_owned()))?;
                let id=call.params.get("bash_id").and_then(Value::as_str);
                if name=="kill_bash" {return tool_result(execute_kill_bash(&mut manager,id,call.params.get("all").and_then(Value::as_bool).unwrap_or(false)));}
                let id=id.ok_or_else(||ToolError::Message("bash_id must be a string".to_owned()))?;let resolved=manager.resolve_id(id).unwrap_or_else(||id.to_owned());let runtime=manager.get(&resolved);
                match name {
                    "bash_output"=>{if call.params.get("view").and_then(Value::as_str)==Some("screen") {return Err(ToolError::Message("Native terminal screen projection is not ported.".to_owned()));}tool_result(execute_bash_output(runtime.as_deref(),id,call.params.get("filter").and_then(Value::as_str)).map_err(|error|ToolError::Message(error.to_string()))?)},
                    "bash_input"=>{let keys=call.params.get("keys").and_then(Value::as_array).map(|keys|keys.iter().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>()).unwrap_or_default();tool_result(execute_bash_input(runtime,BashInputInput {bash_id:id,input:call.params.get("input").and_then(Value::as_str),keys:&keys,submit:call.params.get("submit").and_then(Value::as_bool)}))},
                    "bash_resize"=>tool_result(execute_bash_resize(runtime.as_deref(),id,call.params.get("cols").and_then(Value::as_f64).unwrap_or(f64::NAN),call.params.get("rows").and_then(Value::as_f64).unwrap_or(f64::NAN))),_=>unreachable!(),
                }
            })})));
        }
        let cleanup=Arc::clone(&manager);
        api.on(EventKind::SessionShutdown,Arc::new(move |_,_| {let manager=Arc::clone(&cleanup);Box::pin(async move {manager.lock().map_err(|_|ExtensionFailure::new("terminal manager state poisoned"))?.teardown().map_err(|error|ExtensionFailure::new(error.to_string()))?;Ok(EventResult::None)})}));
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
    #[test] fn native_companions_register_flat_schemas_and_shutdown() {let mut api=ExtensionApi::new(LoadedExtension::new("terminal","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());TerminalExtension.register(&mut api);assert_eq!(api.registered.tools.iter().map(|tool|tool.definition.name.as_str()).collect::<Vec<_>>(),vec!["bash","bash_output","bash_input","bash_resize","kill_bash"]);for tool in &api.registered.tools {assert_eq!(tool.definition.parameters["type"],"object");assert!(tool.definition.parameters.get("properties").is_some());}assert_eq!(api.registered.handlers[&EventKind::SessionShutdown].len(),1);}
}
