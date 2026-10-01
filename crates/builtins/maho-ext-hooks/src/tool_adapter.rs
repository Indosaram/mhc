use maho_ext_api::types::{ToolCallEvent,ToolCallEventResult,ToolResultEvent,ToolResultEventResult,ToolContent};
use serde_json::{Value,json};
use crate::dispatcher::{HookDispatchDecision,HookDispatchResult};
pub const PRE_TOOL_BLOCK_REASON:&str="PreToolUse hook blocked the tool call.";
pub const POST_TOOL_BLOCK_REASON:&str="PostToolUse hook flagged the tool result.";
pub fn build_pre_tool_use_hook_input(event:&ToolCallEvent,cwd:&str,session_id:&str)->Value {json!({"event":"PreToolUse","toolName":event.tool_name,"toolInput":event.input,"cwd":cwd,"session_id":session_id,"hook_event_name":"PreToolUse","tool_name":event.tool_name,"tool_input":event.input,"tool_use_id":event.tool_call_id})}
pub fn build_post_tool_use_hook_input(event:&ToolResultEvent,cwd:&str,session_id:&str)->Value {let output=json!({"content":event.content,"details":event.details,"is_error":event.is_error});json!({"event":"PostToolUse","toolName":event.tool_name,"toolInput":event.input,"toolOutput":output,"cwd":cwd,"session_id":session_id,"hook_event_name":"PostToolUse","tool_name":event.tool_name,"tool_input":event.input,"tool_response":output,"tool_use_id":event.tool_call_id})}
pub fn tool_contexts_from_result(result:&HookDispatchResult)->Vec<String> {result.summaries.iter().filter_map(|summary|summary.output.get("additionalContext").and_then(Value::as_str).map(str::to_owned)).collect()}
fn block_reason(result:&HookDispatchResult,default:&str)->String {let HookDispatchDecision::Block {source,reason,..}=&result.decision else {return default.to_owned();};let blocker=result.summaries.iter().find(|summary|summary.handler.source.source_path==source.source_path&&matches!(summary.output.get("decision").and_then(Value::as_str),Some("block"|"deny")));if blocker.is_some_and(|summary|summary.run.exit_code==Some(2)) {default.to_owned()} else {reason.clone().unwrap_or_else(||default.to_owned())}}
pub fn apply_pre_tool_use_result(event:&mut ToolCallEvent,result:&HookDispatchResult)->Option<ToolCallEventResult> {match &result.decision {
    HookDispatchDecision::Block {..}=>Some(ToolCallEventResult {block:Some(true),reason:Some(block_reason(result,PRE_TOOL_BLOCK_REASON)),..Default::default()}),
    HookDispatchDecision::Ask {fallback_reason,..}=>Some(ToolCallEventResult {block:Some(true),reason:Some(fallback_reason.clone()),..Default::default()}),
    HookDispatchDecision::Allow {updated_input:Some(input),..} if input.is_object()=>{event.input=input.clone();None},_=>None,
}}
fn normalize_output(value:&Value)->Option<(Vec<ToolContent>,Option<Value>)> {
    if let Some(text)=value.as_str() {return Some((vec![ToolContent::text(text)],None));}
    if value.is_array() {return serde_json::from_value(value.clone()).ok().map(|content|(content,None));}
    let raw=value.as_object()?;let content=raw.get("content")?;let content=if let Some(text)=content.as_str() {vec![ToolContent::text(text)]} else {serde_json::from_value(content.clone()).ok()?};Some((content,raw.get("details").cloned()))
}
pub fn apply_post_tool_use_result(event:&ToolResultEvent,result:&HookDispatchResult,pre_tool_contexts:&[String])->Option<ToolResultEventResult> {
    if matches!(result.decision,HookDispatchDecision::Block {..}) {let mut content=vec![ToolContent::text(block_reason(result,POST_TOOL_BLOCK_REASON))];content.extend(pre_tool_contexts.iter().map(ToolContent::text));content.extend(tool_contexts_from_result(result).into_iter().map(ToolContent::text));return Some(ToolResultEventResult {content:Some(content),details:event.details.clone(),is_error:Some(true),usage:None});}
    let mut content=event.content.clone();let mut details=event.details.clone();let mut replaced=false;let mut contexts=Vec::new();
    for summary in &result.summaries {if let Some(updated)=summary.output.get("updatedToolOutput").and_then(normalize_output) {content=updated.0;details=updated.1;replaced=true;}
        if let Some(context)=summary.output.get("additionalContext").and_then(Value::as_str) {contexts.push(context.to_owned());}}
    if !replaced&&contexts.is_empty()&&pre_tool_contexts.is_empty() {return None;}
    content.extend(contexts.iter().map(ToolContent::text));content.extend(pre_tool_contexts.iter().map(ToolContent::text));Some(ToolResultEventResult {content:Some(content),details,is_error:None,usage:None})
}
#[cfg(test)]
mod tests {use super::*;#[test] fn wire_aliases_and_input_id() {let event=ToolCallEvent {tool_call_id:"c1".to_owned(),tool_name:"bash".to_owned(),input:json!({"command":"pwd"})};let input=build_pre_tool_use_hook_input(&event,"/repo","s1");assert_eq!(input["toolInput"],input["tool_input"]);assert_eq!(input["tool_use_id"],"c1");}#[test] fn normalizes_string_array_and_object_outputs() {assert_eq!(normalize_output(&json!("ok")),Some((vec![ToolContent::text("ok")],None)));assert_eq!(normalize_output(&json!({"content":"ok","details":{"n":1}})),Some((vec![ToolContent::text("ok")],Some(json!({"n":1})))));assert!(normalize_output(&json!([{"type":"image","mimeType":"image/png"}])).is_none());}}
