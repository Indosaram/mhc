use std::sync::{Arc, Mutex};
use maho_tools::definition::{ToolDefinition, ToolError, ToolExecutionMode, ToolResult};
use serde_json::{Value, json};
use crate::{engine::{bm25::{Bm25Result, Bm25SearchOptions},document::ToolSearchSource},service::{HiddenToolHint,ToolSearchService}};
pub const TOOL_SEARCH_TOOL_NAME: &str="tool_search";
pub fn prepare_tool_search_arguments(mut args: Value) -> Value {
    if let Some(obj)=args.as_object_mut() && let Some(server)=obj.remove("server") {
        if obj.get("source").is_none_or(Value::is_null) { obj.insert("source".into(),json!("mcp")); }
        if obj.get("group").is_none_or(Value::is_null) { obj.insert("group".into(),server); }
    }
    args
}
pub fn create_tool_search_tool(service: Arc<Mutex<ToolSearchService>>) -> ToolDefinition {
    let mut definition=ToolDefinition::new(TOOL_SEARCH_TOOL_NAME,"Search the catalog of deferred tools by capability. Returns matching tool names with their parameter schemas and never changes your active tool set; call a returned tool by name and it activates on that first call.",json!({"type":"object","properties":{"query":{"type":"string","description":"Natural-language description of the capability you need."},"source":{"anyOf":[{"const":"mcp","type":"string"},{"const":"extension","type":"string"}],"description":"Optional: restrict the search to MCP or extension tools."},"group":{"type":"string","description":"Optional: restrict the search to one catalog group."}},"required":["query"]}),Arc::new(move |call| {
        let service=Arc::clone(&service);
        Box::pin(async move {
            let query=call.params.get("query").and_then(Value::as_str).ok_or_else(||ToolError::Message("query must be a string".into()))?;
            let source=call.params.get("source").and_then(Value::as_str).map(|s| match s { "mcp"=>Ok(ToolSearchSource::Mcp),"extension"=>Ok(ToolSearchSource::Extension),_=>Err(ToolError::Message("source must be mcp or extension".into())) }).transpose()?;
            let group=call.params.get("group").and_then(Value::as_str);
            let mut service=service.lock().map_err(|_|ToolError::Message("ToolSearchService lock poisoned".into()))?;
            let matches=service.search(query,5,&Bm25SearchOptions { source,group:group.map(str::to_owned),..Default::default() });
            let text=build_tool_search_result_text(query,&matches,&service.hidden_tool_hints(query),source,group,|name|service.get_tool_parameters(name));
            Ok(ToolResult { content:vec![maho_tools::definition::ToolContent::text(text)],details:Some(json!({"matched":matches.iter().map(|m|&m.name).collect::<Vec<_>>(),"query":query})) })
        })
    }));
    definition.label="Tool search".into(); definition.execution_mode=Some(ToolExecutionMode::Parallel);
    definition.prompt_snippet=Some("Search deferred tool catalogs by capability; call a returned tool by name to use it.".into());
    definition.prepare_arguments=Some(Arc::new(|args|Ok(prepare_tool_search_arguments(args)))); definition
}
pub fn build_tool_search_result_text(query: &str, matches: &[Bm25Result], hints: &[HiddenToolHint], source: Option<ToolSearchSource>, group: Option<&str>, parameters_of: impl Fn(&str)->Option<Value>) -> String {
    let mut scope=Vec::new();
    if let Some(source)=source { scope.push(format!("source \"{}\"",match source { ToolSearchSource::Mcp=>"mcp",ToolSearchSource::Extension=>"extension" })); }
    if let Some(group)=group { scope.push(format!("group \"{group}\"")); }
    let scope=if scope.is_empty() { String::new() } else { format!(" in {}",scope.join(" in ")) };
    let hint_lines:Vec<_>=hints.iter().map(|h|format!("- {}: {}",h.name,h.hint)).collect();
    if matches.is_empty() {
        let head=format!("No catalog tools matched \"{query}\"{scope}. Your active tool set is unchanged.");
        if hints.is_empty() { return format!("{head} Try different keywords or a broader query."); }
        return format!("{head}\n\nThe query names a tool that is hidden in this session:\n{}",hint_lines.join("\n"));
    }
    let mut lines=vec![format!("Found {} tool(s) matching \"{query}\"{scope}. Nothing was activated; call one by name and it activates on that first call:",matches.len()),String::new()];
    for m in matches {
        let description=m.doc.description.as_deref().map(|s|s.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|s|!s.is_empty()).unwrap_or_else(||"(no description)".into());
        let description=if description.encode_utf16().count()>160 { format!("{}...",String::from_utf16_lossy(&description.encode_utf16().take(157).collect::<Vec<_>>())) } else { description };
        lines.push(format!("- {} — {description}",m.name));
        if let Some(parameters)=parameters_of(&m.name).filter(Value::is_object) { lines.push(format!("  parameters: {parameters}")); }
    }
    if !hint_lines.is_empty() { lines.extend([String::new(),"The query also names a tool that is hidden in this session:".into()]); lines.extend(hint_lines); } lines.join("\n")
}
