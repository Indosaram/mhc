use std::collections::BTreeMap;
use serde_json::{Value,Map,json};
pub const BUILTIN_SDK_TOOLS:[&str;6]=["Read","Write","Edit","Bash","Grep","Glob"];
pub const CUSTOM_TOOLS_MCP_PREFIX:&str="mcp__custom-tools__";
pub const HOST_TOOL_POLICY_FINGERPRINT:&str="host-tool-denial-v2";
pub const TOOL_EXECUTION_DENIED_MESSAGE:&str="Senpi executes this tool on the host and returns its result as the next user message. Wait for that result; this denial is not a failure.";
fn builtin(name:&str)->Option<&'static str> {match name {"read"=>Some("Read"),"write"=>Some("Write"),"edit"=>Some("Edit"),"bash"=>Some("Bash"),"grep"=>Some("Grep"),"find"|"glob"=>Some("Glob"),_=>None}}
pub fn map_pi_tool_name(name:&str,custom:&BTreeMap<String,String>)->String {
    let normalized=name.to_lowercase();if let Some(mapped)=custom.get(name).or_else(||custom.get(&normalized)) {return mapped.clone();}
    if let Some(mapped)=builtin(&normalized) {return mapped.into();}
    name.split(|c:char|!c.is_ascii_alphanumeric()).filter(|p|!p.is_empty()).map(|part|format!("{}{}",part[..1].to_uppercase(),&part[1..])).collect()
}
pub fn map_sdk_tool_name(name:&str,custom:&BTreeMap<String,String>)->String {
    let normalized=name.to_lowercase();let built=match normalized.as_str() {"read"|"write"|"edit"|"bash"|"grep"=>Some(normalized.as_str()),"glob"=>Some("find"),_=>None};
    built.map(str::to_owned).or_else(||custom.get(name).or_else(||custom.get(&normalized)).cloned()).unwrap_or_else(||if normalized.starts_with(CUSTOM_TOOLS_MCP_PREFIX) {name[CUSTOM_TOOLS_MCP_PREFIX.len()..].into()}else {name.into()})
}
#[derive(Default)]
pub struct ResolvedTools {pub sdk_tools:Vec<String>,pub custom_tools:Vec<Value>,pub to_sdk:BTreeMap<String,String>,pub to_pi:BTreeMap<String,String>}
pub fn resolve_tools(tools:Option<&[Value]>)->ResolvedTools {
    let mut result=ResolvedTools::default();let Some(tools)=tools else {result.sdk_tools=BUILTIN_SDK_TOOLS.map(str::to_owned).into();return result;};
    for tool in tools {let name=tool["name"].as_str().expect("tool name");let normalized=name.to_lowercase();
        if let Some(sdk)=builtin(&normalized) {if !result.sdk_tools.iter().any(|s|s==sdk) {result.sdk_tools.push(sdk.into());}continue;}
        let sdk=format!("{CUSTOM_TOOLS_MCP_PREFIX}{name}");result.custom_tools.push(tool.clone());result.to_sdk.insert(name.into(),sdk.clone());result.to_sdk.insert(normalized,sdk.clone());result.to_pi.insert(sdk.clone(),name.into());result.to_pi.insert(sdk.to_lowercase(),name.into());
    }result
}
pub fn map_tool_args(name:&str,args:Option<&Map<String,Value>>)->Map<String,Value> {
    let empty=Map::new();let input=args.unwrap_or(&empty);let get=|keys:&[&str]|keys.iter().find_map(|key|input.get(*key).filter(|v|!v.is_null())).cloned();let mut output=Map::new();
    let fields:&[(&str,&[&str])]=match name.to_lowercase().as_str() {
        "read"=>&[("path",&["file_path","path"]),("offset",&["offset"]),("limit",&["limit"])],
        "write"=>&[("path",&["file_path","path"]),("content",&["content"])],
        "edit"=>&[("path",&["file_path","path"])],
        "bash"=>&[("command",&["command"]),("timeout",&["timeout"])],
        "grep"=>&[("pattern",&["pattern"]),("path",&["path"]),("glob",&["glob"]),("ignoreCase",&["-i","ignoreCase"]),("context",&["context","-C"]),("limit",&["head_limit","limit"])],
        "glob"|"find"=>&[("pattern",&["pattern"]),("path",&["path"]),("limit",&["limit"])],_=>return input.clone(),
    };
    for (target,keys) in fields {if let Some(value)=get(keys) {output.insert((*target).into(),value);}}
    if name.eq_ignore_ascii_case("edit") {let edits=input.get("edits").filter(|v|v.is_array()).cloned().unwrap_or_else(|| {let mut edit=Map::new();for (key,keys) in [("oldText",&["old_string","oldText","old_text"][..]),("newText",&["new_string","newText","new_text"][..])] {if let Some(value)=get(keys) {edit.insert(key.into(),value);}}json!([edit])});output.insert("edits".into(),edits);}
    if name.eq_ignore_ascii_case("bash")&&let Some(timeout)=input.get("timeout").and_then(Value::as_f64) {output.insert("timeout".into(),json!(timeout/1000.0));}output
}
pub fn deny_tool_execution()->Value {json!({"behavior":"deny","message":TOOL_EXECUTION_DENIED_MESSAGE})}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tool_selection_and_custom_names() {let tools=[json!({"name":"read"}),json!({"name":"bash"}),json!({"name":"find"}),json!({"name":"repoSearch"})];let resolved=resolve_tools(Some(&tools));assert_eq!(resolved.sdk_tools,["Read","Bash","Glob"]);assert_eq!(map_pi_tool_name("repoSearch",&resolved.to_sdk),"mcp__custom-tools__repoSearch");assert_eq!(map_sdk_tool_name("mcp__custom-tools__repoSearch",&resolved.to_pi),"repoSearch");assert!(resolve_tools(Some(&[])).sdk_tools.is_empty());assert_eq!(resolve_tools(None).sdk_tools,BUILTIN_SDK_TOOLS);}
    #[test]
    fn argument_conversion_preserves_host_contract() {for (name,input,expected) in [("Read",json!({"file_path":"src","offset":4,"limit":12}),json!({"path":"src","offset":4,"limit":12})),("Edit",json!({"file_path":"src","old_string":"before","new_string":"after","replace_all":true}),json!({"path":"src","edits":[{"oldText":"before","newText":"after"}]})),("Grep",json!({"pattern":"n","-i":true,"-C":2,"head_limit":5}),json!({"pattern":"n","ignoreCase":true,"context":2,"limit":5}))] {assert_eq!(map_tool_args(name,input.as_object()),expected.as_object().expect("expected").clone());}assert_eq!(map_tool_args("Bash",json!({"command":"test","timeout":30000}).as_object())["timeout"].as_f64(),Some(30.0));assert_eq!(deny_tool_execution()["behavior"],"deny");}
}
