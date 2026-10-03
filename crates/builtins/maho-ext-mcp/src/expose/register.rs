use std::{sync::{Arc,atomic::{AtomicU64,Ordering}},path::PathBuf};
use maho_ext_api::{ToolDefinition,ToolExecutionMode,ToolContent,ToolResult,ToolError};
use serde_json::{Value,json};
use crate::{catalog::McpToolCatalogEntry,config_schema::OutputGuardSettings,guard::output_guard::{McpOutputArtifacts,McpOutputGuardOptions,apply_mcp_output_guard}};
use super::schema_compat::{McpToolNameEntry,build_mcp_tool_names,convert_json_schema_to_type_box,map_mcp_tool_result};
#[derive(Clone)]
pub struct McpNamedCatalogEntry {pub entry:McpToolCatalogEntry,pub name:String}
static NEXT_PROGRESS_TOKEN:AtomicU64=AtomicU64::new(0);
pub fn map_mcp_catalog_names(entries:&[McpToolCatalogEntry])->Vec<McpNamedCatalogEntry> {
    let collator=icu_collator::Collator::try_new(Default::default(),Default::default()).expect("compiled collation data is available");
    let mut entries=entries.to_vec();entries.sort_by(|left,right|collator.compare(&left.server,&right.server).then_with(||collator.compare(&left.tool,&right.tool)));
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
                let operation=||async {
                if let Some(ensure_fresh)=&entry.ensure_fresh && let Err(error)=ensure_fresh().await {
                    return Err(entry.runtime.as_ref().and_then(|runtime|crate::health::mark_mcp_connection_needs_auth(&runtime.connection,&error)).unwrap_or(error));
                }
                let client=if let Some(runtime)=&entry.runtime {
                    runtime.health.ensure_connection(&runtime.connection).await?;
                    if let Some(ensure_connected)=&entry.ensure_connected {ensure_connected().await?;}
                    runtime.connection.client()?
                }else{
                    if let Some(ensure_connected)=&entry.ensure_connected {ensure_connected().await?;}
                    entry.client.clone().ok_or_else(||crate::errors::McpError::new(crate::errors::McpErrorKind::Connect,"MCP catalog entry has no connection"))?
                };
                let token=format!("native:{}:{}:{}:{}",entry.server,entry.tool,call.id,NEXT_PROGRESS_TOKEN.fetch_add(1,Ordering::Relaxed));
                let mut notifications=client.notifications.subscribe();
                let mut notifications_open=true;
                let request=client.request_with_signal("tools/call",json!({"name":entry.tool,"arguments":params,"_meta":{"progressToken":token}}),entry.request_timeout,&call.signal);tokio::pin!(request);
                let result=loop {tokio::select! {
                    biased;
                    notification=notifications.recv(),if notifications_open=>{
                        let value=match notification {Ok(value)=>value,Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>continue,Err(tokio::sync::broadcast::error::RecvError::Closed)=>{notifications_open=false;continue}};
                        let progress=value.get("params").unwrap_or(&Value::Null);
                        if value.get("method").and_then(Value::as_str)==Some("notifications/progress") && progress.get("progressToken")==Some(&json!(token)) && let Some(update)=&call.on_update {
                            let total=progress.get("total").map_or_else(String::new,|total|format!("/{total}"));let message=progress.get("message").and_then(Value::as_str).map_or_else(String::new,|message|format!(" {message}"));
                            update(ToolResult {content:vec![ToolContent::text(format!("{}/{} progress {}{total}{message}",entry.server,entry.tool,progress.get("progress").unwrap_or(&Value::Null)))],details:Some(json!({"progress":progress,"server":entry.server,"tool":entry.tool}))}).map_err(|error|crate::errors::McpError::new(crate::errors::McpErrorKind::ToolExec,error.to_string()))?;
                        }
                    }
                    result=&mut request=>{call.signal.check().map_err(|error|crate::errors::McpError::new(crate::errors::McpErrorKind::ToolExec,error.to_string()))?;break result?;},
                }};
                Ok::<_,crate::errors::McpError>(result)
                };
                let result=if let Some(runtime)=&entry.runtime {
                    runtime.lifecycle.run_call(crate::health::with_mcp_session_expiry_retry(&runtime.connection,||crate::health::with_mcp_retriable_failed_send_retry(&runtime.connection,operation))).await
                }else{operation().await}.map_err(|error|ToolError::Message(format!("ToolExecError: {error}")))?;
                mapped_guarded_result(&entry,&result,entry.agent_dir.as_deref().unwrap_or(&agent_dir),entry.artifacts.as_ref().unwrap_or(&artifacts),entry.output_guard.as_ref().or(output_guard.as_ref()))
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
