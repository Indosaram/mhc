use std::path::{Component,Path,PathBuf};
use serde_json::Value;
use super::constants::TRACKED_BUILTIN_TOOLS;
pub fn is_tracked_tool(tool_name:&str)->bool {TRACKED_BUILTIN_TOOLS.contains(&tool_name)}
pub fn extract_tool_paths(tool_name:&str,is_error:bool,input:&Value,details:Option<&Value>,cwd:&Path)->Vec<PathBuf>{
    if is_error || !is_tracked_tool(tool_name){return Vec::new();}
    let mut result=Vec::new();
    let mut add=|value:Option<&str>|{
        if let Some(value)=value.filter(|value|!value.is_empty()) {
            let path=Path::new(value);
            let path=if path.is_absolute(){path.to_owned()}else{resolve_path(&cwd.join(path))};
            if !result.contains(&path){result.push(path);}
        }
    };
    if tool_name=="read" || tool_name=="edit" {add(details.and_then(|v|v.get("filePath")).and_then(Value::as_str));add(input.get("path").and_then(Value::as_str));}
    if tool_name=="write" {add(input.get("filePath").and_then(Value::as_str));add(input.get("path").and_then(Value::as_str));}
    result
}
pub fn resolve_path(path:&Path)->PathBuf{
    let mut result=PathBuf::new();
    for component in path.components(){match component{Component::CurDir=>{},Component::ParentDir=>{result.pop();},Component::RootDir|Component::Prefix(_)|Component::Normal(_)=>result.push(component.as_os_str())}}
    result
}
