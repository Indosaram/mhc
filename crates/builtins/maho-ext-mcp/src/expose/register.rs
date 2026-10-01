use std::{sync::Arc,path::PathBuf};
use maho_ext_api::{ToolDefinition,ToolExecutionMode,ToolContent,ToolResult,ToolError};
use serde_json::{Value,json};
use crate::{catalog::McpToolCatalogEntry,config_schema::OutputGuardSettings,guard::output_guard::{McpOutputArtifacts,McpOutputGuardOptions,apply_mcp_output_guard}};
use super::schema_compat::{McpToolNameEntry,build_mcp_tool_names,convert_json_schema_to_type_box,map_mcp_tool_result};
#[derive(Clone)]
pub struct McpNamedCatalogEntry {pub entry:McpToolCatalogEntry,pub name:String}
pub fn map_mcp_catalog_names(entries:&[McpToolCatalogEntry])->Vec<McpNamedCatalogEntry> {
    let mut entries=entries.to_vec();entries.sort_by(|left,right|left.server.cmp(&right.server).then(left.tool.cmp(&right.tool)));
    let names=build_mcp_tool_names(&entries.iter().map(|entry|McpToolNameEntry {server_name:entry.server.clone(),tool_name:entry.tool.clone()}).collect::<Vec<_>>(),None);
    entries.into_iter().zip(names).map(|(entry,name)|McpNamedCatalogEntry {entry,name}).collect()
}
pub fn build_mcp_tool_definitions(entries:&[McpToolCatalogEntry],agent_dir:PathBuf,artifacts:Arc<McpOutputArtifacts>,output_guard:Option<OutputGuardSettings>)->Vec<ToolDefinition> {
    map_mcp_catalog_names(entries).into_iter().map(|named|{
        let entry=named.entry;let label=format!("{}/{}",entry.server,entry.tool);
        let description=entry.description.clone().unwrap_or_else(||format!("MCP tool {label}"));
        let schema=convert_json_schema_to_type_box(&entry.schema).schema;
        let agent_dir=agent_dir.clone();let artifacts=artifacts.clone();let output_guard=output_guard.clone();
        let mut tool=ToolDefinition::new(&named.name,&description,schema,Arc::new(move |call|{
            let entry=entry.clone();let agent_dir=agent_dir.clone();let artifacts=artifacts.clone();let output_guard=output_guard.clone();
            Box::pin(async move {
                call.signal.check()?;
                let params=if call.params.is_object(){call.params}else{json!({})};
                let result=tokio::select! {
                    result=entry.client.request("tools/call",json!({"name":entry.tool,"arguments":params}),entry.request_timeout)=>result.map_err(|error|ToolError::Message(format!("ToolExecError: {error}")))?,
                    ()=call.signal.cancelled()=>return Err(ToolError::Aborted),
                };
                mapped_guarded_result(&entry,&result,&agent_dir,&artifacts,output_guard.as_ref())
            })
        }));
        tool.label=label;tool.execution_mode=Some(ToolExecutionMode::Parallel);tool
    }).collect()
}
pub fn mapped_guarded_result(entry:&McpToolCatalogEntry,result:&Value,agent_dir:&std::path::Path,artifacts:&McpOutputArtifacts,output_guard:Option<&OutputGuardSettings>)->Result<ToolResult,ToolError> {
    let normalized=if result.get("content").is_some() || result.get("structuredContent").is_some() || result.get("isError").is_some(){result.clone()}else{json!({"structuredContent":result.get("toolResult").cloned().unwrap_or(Value::Null)})};
    let mapped=map_mcp_tool_result(&normalized);
    if mapped.get("ok")==Some(&Value::Bool(false)){return Err(ToolError::Message(mapped["error"]["message"].as_str().unwrap_or("MCP tool returned an error result.").into()));}
    let blocks=mapped.get("content").and_then(Value::as_array).map_or(&[][..],Vec::as_slice);
    let guarded=apply_mcp_output_guard(blocks,McpOutputGuardOptions {agent_dir,artifacts:Some(artifacts),server:&entry.server,output_guard});
    let content=guarded.into_iter().map(|block|match block.get("type").and_then(Value::as_str) {
        Some("text")=>ToolContent::text(block.get("text").and_then(Value::as_str).unwrap_or("")),
        Some("image")=>ToolContent::Image {data:block["data"].as_str().unwrap_or("").into(),mime_type:block["mimeType"].as_str().unwrap_or("").into()},
        _=>ToolContent::text(block.to_string()),
    }).collect::<Vec<_>>();
    let preview=content.iter().map(|block|match block {ToolContent::Text {text,..}=>text.clone(),ToolContent::Image {mime_type,..}=>format!("[{mime_type} image]")}).collect::<Vec<_>>().join(" ");
    let preview=preview.trim();let preview=if preview.is_empty(){"(empty result)".into()}else if preview.encode_utf16().count()>120 {format!("{}...",String::from_utf16_lossy(&preview.encode_utf16().take(117).collect::<Vec<_>>()))}else{preview.to_owned()};
    Ok(ToolResult {content,details:Some(json!({"preview":preview,"server":entry.server,"tool":entry.tool}))})
}
