use serde_json::{Value, json};
use crate::engine::{bm25::Bm25Result,document::ToolSearchSource};
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct HiddenToolHint { pub name:String,pub hint:String }
pub const TOOL_SEARCH_TOOL_NAME: &str="tool_search";
pub fn parameters()->Value { json!({"type":"object","properties":{"query":{"type":"string","description":"Natural-language description of the capability you need."},"source":{"anyOf":[{"const":"mcp","type":"string"},{"const":"extension","type":"string"}],"description":"Optional: restrict the search to MCP or extension tools."},"group":{"type":"string","description":"Optional: restrict the search to one catalog group."}},"required":["query"]}) }
pub fn create_tool_search_tool(service:std::sync::Arc<std::sync::Mutex<crate::service::ToolSearchService>>)->maho_tools::definition::ToolDefinition {
    use maho_tools::definition::{ToolDefinition,ToolError,ToolResult,ToolContent,ToolExecutionMode}; use std::sync::Arc;
    let mut tool=ToolDefinition::new(TOOL_SEARCH_TOOL_NAME,"Search the catalog of deferred tools by capability. Returns matching tool names with their parameter schemas and never changes your active tool set; call a returned tool by name and it activates on that first call.",parameters(),Arc::new(move |call| {
        let service=service.clone(); Box::pin(async move {
            let query=call.params.get("query").and_then(Value::as_str).ok_or_else(||ToolError::Message("query is required".into()))?;
            let source=match call.params.get("source").and_then(Value::as_str) { Some("mcp")=>Some(ToolSearchSource::Mcp),Some("extension")=>Some(ToolSearchSource::Extension),_=>None }; let group=call.params.get("group").and_then(Value::as_str);
            let mut service=service.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let matches=service.search(query,5,&crate::engine::bm25::Bm25SearchOptions{source,group:group.map(String::from),..Default::default()}).map_err(|error|ToolError::Message(error.to_string()))?;
            let hints=service.hidden_tool_hints(query); let mut parameters=std::collections::BTreeMap::new(); for item in &matches { parameters.insert(item.name.clone(),service.get_tool_parameters(&item.name).map_err(|error|ToolError::Message(error.to_string()))?); }
            let text=build_tool_search_result_text(query,&matches,&hints,source,group,|name|parameters.get(name).cloned().flatten());
            Ok(ToolResult{content:vec![ToolContent::text(text)],details:Some(json!({"matched":matches.iter().map(|item|&item.name).collect::<Vec<_>>(),"query":query}))})
        })
    }));
    tool.label="Tool search".into(); tool.execution_mode=Some(ToolExecutionMode::Parallel); tool.prepare_arguments=Some(Arc::new(|args|Ok(prepare_tool_search_arguments(args)))); tool.prompt_snippet=Some("Search deferred tool catalogs by capability; call a returned tool by name to use it.".into()); tool
}
pub fn prepare_tool_search_arguments(mut args: Value) -> Value {
    if let Some(obj)=args.as_object_mut() {
        let server=obj.remove("server");
        if (obj.contains_key("source") || server.is_some()) && obj.get("source").is_none_or(Value::is_null) { obj.insert("source".into(),json!("mcp")); }
        if (obj.contains_key("group") || server.is_some()) && obj.get("group").is_none_or(Value::is_null) { match server { Some(server)=>{obj.insert("group".into(),server);},None=>{obj.remove("group");} } }
    }
    args
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
#[cfg(test)]
mod argument_tests {
    use super::*;
    #[test] fn null_source_without_server_still_defaults_to_mcp() { assert_eq!(prepare_tool_search_arguments(json!({"query":"files","source":null})),json!({"query":"files","source":"mcp"})); }
    #[test] fn null_group_without_server_is_omitted() { assert_eq!(prepare_tool_search_arguments(json!({"query":"files","group":null})),json!({"query":"files"})); }
}
