use std::{path::PathBuf,sync::Arc,time::Duration};
use serde_json::{Value,json};
use crate::{transport_sdk::McpClient,errors::{McpError,McpErrorKind},config_schema::OutputGuardSettings,guard::output_guard::{McpOutputArtifacts,McpOutputGuardOptions,apply_mcp_output_guard}};
pub struct McpResourceServer {
    pub server:String,pub client:Arc<McpClient>,pub agent_dir:PathBuf,pub artifacts:Arc<McpOutputArtifacts>,
    pub output_guard:Option<OutputGuardSettings>,pub request_timeout:Duration,pub resources:Vec<Value>,
}
pub async fn ensure_mcp_resource_subscriptions(client:Arc<McpClient>,resources:&[Value],timeout:Duration) {
    if client.server_capabilities.read().await.pointer("/resources/subscribe")!=Some(&Value::Bool(true)){return;}
    let mut pending=tokio::task::JoinSet::new();
    for resource in resources {
        if let Some(uri)=resource.get("uri").and_then(Value::as_str) {
            let client=client.clone();let uri=uri.to_owned();
            pending.spawn(async move {let _=client.request("resources/subscribe",json!({"uri":uri}),timeout).await;});
        }
    }
    while pending.join_next().await.is_some() {}
}
pub async fn read_mcp_resource_as_text(server:&McpResourceServer,uri:&str)->Result<String,McpError> {
    let result=server.client.request("resources/read",json!({"uri":uri}),server.request_timeout).await.map_err(|error| {
        let mut failure=McpError::new(McpErrorKind::ToolExec,format!("Failed to read MCP resource {uri}: {error}"));failure.phase=Some("call".into());failure.server_name=Some(server.server.clone());failure
    })?;
    let text=flatten_resource_contents(result.get("contents").and_then(Value::as_array).map_or(&[],Vec::as_slice));
    let guarded=apply_mcp_output_guard(&[json!({"type":"text","text":text})],McpOutputGuardOptions {agent_dir:&server.agent_dir,artifacts:Some(&server.artifacts),server:&server.server,output_guard:server.output_guard.as_ref()});
    Ok(guarded.iter().map(|block|block.get("text").and_then(Value::as_str).unwrap_or("")).collect::<Vec<_>>().join("\n"))
}
pub fn flatten_resource_contents(contents:&[Value])->String {
    contents.iter().map(|content| {
        if let Some(text)=content.get("text").and_then(Value::as_str){return text.to_owned();}
        let bytes=content.get("blob").and_then(Value::as_str).map_or(0,|s|(s.encode_utf16().count()*3+2)/4);
        format!("[binary resource {} ({}), ~{bytes} bytes]",content.get("uri").and_then(Value::as_str).unwrap_or("undefined"),content.get("mimeType").and_then(Value::as_str).unwrap_or("unknown"))
    }).collect::<Vec<_>>().join("\n")
}
pub struct McpMentionExpansion {pub text:String,pub changed:bool,pub notices:Vec<String>}
pub async fn expand_mcp_resource_mentions(text:&str,servers:&[McpResourceServer])->Result<McpMentionExpansion,regex::Error> {
    let pattern=regex::Regex::new(r"@mcp:([A-Za-z0-9._-]+)/(\S+)")?;let mut expanded=text.to_owned();let mut notices=Vec::new();
    for capture in pattern.captures_iter(text) {
        let (Some(mention),Some(name),Some(uri))=(capture.get(0),capture.get(1),capture.get(2)) else{continue;};
        let Some(server)=servers.iter().find(|server|server.server==name.as_str()) else{notices.push(format!("@mcp mention left as-is: unknown server '{}'.",name.as_str()));continue;};
        match read_mcp_resource_as_text(server,uri.as_str()).await {
            Ok(body)=>expanded=expanded.replacen(mention.as_str(),&format!("<mcp-resource server=\"{}\" uri=\"{}\">\n{body}\n</mcp-resource>",name.as_str(),uri.as_str()),1),
            Err(error)=>notices.push(format!("@mcp mention left as-is: {error}")),
        }
    }
    Ok(McpMentionExpansion {changed:expanded!=text,text:expanded,notices})
}
