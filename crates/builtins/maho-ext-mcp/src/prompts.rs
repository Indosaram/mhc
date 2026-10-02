use std::{collections::BTreeSet,sync::Arc,time::Duration};
use maho_ext_api::{ExtensionApi,ExtensionUi,ExtensionUiDialogOptions,NotificationType};
use serde_json::{Map,Value,json};
use crate::connection::ServerConnection;

#[derive(Clone)]
pub struct McpPromptServer {pub server:String,pub connection:Arc<ServerConnection>,pub request_timeout:Duration,pub prompts:Vec<Value>}
pub fn register_mcp_prompt_commands(api:&mut ExtensionApi,servers:&[McpPromptServer],registered:&mut BTreeSet<String>)->Vec<String> {
    let mut added=Vec::new();
    for server in servers {
        for prompt in &server.prompts {
            let Some(name)=prompt.get("name").and_then(Value::as_str) else{continue;};
            let command=format!("mcp:{}:{name}",server.server);
            if !registered.insert(command.clone()){continue;}
            added.push(command.clone());
            let description=prompt.get("description").and_then(Value::as_str).map(str::to_owned).unwrap_or_else(||format!("MCP prompt {name} from {}",server.server));
            let server=server.clone();let prompt=prompt.clone();let name=name.to_owned();
            api.register_command(&command,Some(description),None,Arc::new(move |_,ctx|{
                let server=server.clone();let prompt=prompt.clone();let name=name.clone();
                Box::pin(async move {
                    let args=prompt.get("arguments").and_then(Value::as_array).map_or(&[][..],Vec::as_slice);
                    let Some(collected)=collect_prompt_arguments(args,ctx.ui.as_ref()).await else{return Ok(());};
                    let result=async {server.connection.client()?.request("prompts/get",json!({"name":name,"arguments":collected}),server.request_timeout).await}.await;
                    match result {
                        Ok(result)=>ctx.ui.set_editor_text(&flatten_prompt_messages(result.get("messages").and_then(Value::as_array).map_or(&[],Vec::as_slice))),
                        Err(error)=>ctx.ui.notify(&format!("MCP prompt {name} failed: {error}"),NotificationType::Error),
                    }
                    Ok(())
                })
            }));
        }
    }
    added
}
pub async fn collect_prompt_arguments(declared:&[Value],ui:&dyn ExtensionUi)->Option<Map<String,Value>> {
    let mut collected=Map::new();
    for argument in declared {
        let name=argument.get("name").and_then(Value::as_str).unwrap_or("");
        let required=argument.get("required")==Some(&Value::Bool(true));
        let title=format!("{name}{}",if required {""}else{" (optional)"});
        let value=ui.input(&title,argument.get("description").and_then(Value::as_str),ExtensionUiDialogOptions::default()).await;
        if let Some(value)=value.filter(|value|!value.is_empty()) {collected.insert(name.into(),json!(value));}
        else if required {ui.notify(&format!("MCP prompt cancelled: required argument '{name}' not provided."),NotificationType::Warning);return None;}
    }
    Some(collected)
}
pub fn flatten_prompt_messages(messages:&[Value])->String {
    messages.iter().map(|message|{
        let content=message.get("content").unwrap_or(&Value::Null);
        let text=content.get("text").and_then(Value::as_str).map_or_else(||content.to_string(),str::to_owned);
        if messages.len()>1 {format!("{}: {text}",message.get("role").and_then(Value::as_str).unwrap_or(""))}else{text}
    }).collect::<Vec<_>>().join("\n\n")
}
