use serde_json::{Value, json};
use crate::engine::{bm25::Bm25Result,document::ToolSearchSource};
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct HiddenToolHint { pub name:String,pub hint:String }
pub const TOOL_SEARCH_TOOL_NAME: &str="tool_search";
pub fn prepare_tool_search_arguments(mut args: Value) -> Value {
    if let Some(obj)=args.as_object_mut() && let Some(server)=obj.remove("server") {
        if obj.get("source").is_none_or(Value::is_null) { obj.insert("source".into(),json!("mcp")); }
        if obj.get("group").is_none_or(Value::is_null) { obj.insert("group".into(),server); }
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
