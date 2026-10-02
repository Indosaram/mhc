use std::path::{Path,PathBuf};
use serde_json::Value;
#[derive(Debug)]
pub struct ConfigNotice {pub kind:&'static str,pub server_ids:Vec<String>,pub config_path:PathBuf,pub user_config_path:PathBuf}
pub fn get_config_notices(cwd:&Path,home:&Path) -> Vec<ConfigNotice> {
    let path=cwd.join(".pi/lsp-client.json");
    let Ok(raw)=std::fs::read_to_string(&path) else {return Vec::new();};
    let Ok(v)=serde_json::from_str::<Value>(&raw) else {return Vec::new();};
    let Some(lsp)=v.get("lsp").and_then(Value::as_object) else {return Vec::new();};
    let mut server_ids=Vec::new();
    for (id,entry) in lsp {
        if !entry.is_object() || entry.get("disabled").is_some_and(|v|!v.is_boolean()) || entry.get("command").is_some_and(|v|!v.as_array().is_some_and(|a|a.iter().all(Value::is_string))) || entry.get("env").is_some_and(|v|!v.as_object().is_some_and(|m|m.values().all(Value::is_string))) {return Vec::new();}
        if entry.get("disabled").and_then(Value::as_bool)!=Some(true) && (entry.get("command").is_some() || entry.get("env").is_some()) {server_ids.push(id.clone());}
    }
    if server_ids.is_empty() {Vec::new()} else {vec![ConfigNotice {kind:"untrusted_project_lsp_command",server_ids,config_path:path,user_config_path:home.join(".pi/lsp-client.json")}]}
}
#[cfg(test)]
mod tests {
    use super::*;use serde_json::json;
    fn notices(v:Value)->Vec<ConfigNotice> {let t=tempfile::tempdir().unwrap();std::fs::create_dir(t.path().join(".pi")).unwrap();std::fs::write(t.path().join(".pi/lsp-client.json"),v.to_string()).unwrap();get_config_notices(t.path(),Path::new("/home/fixture"))}
    #[test] fn commands_and_environment_warn() {let n=notices(json!({"lsp":{"a":{"command":["server"]},"b":{"env":{"PATH":"x"}},"disabled":{"command":["server"],"disabled":true},"safe":{}}}));assert_eq!(n.len(),1);assert_eq!(n[0].server_ids,vec!["a","b"]);assert_eq!(n[0].user_config_path,PathBuf::from("/home/fixture/.pi/lsp-client.json"));}
    #[test] fn malformed_entries_reject_whole_config() {for entry in [json!(null),json!({"disabled":"yes"}),json!({"command":[42]}),json!({"env":{"A":42}})] {assert!(notices(json!({"lsp":{"valid":{"command":["server"]},"bad":entry}})).is_empty());}}
    #[test] fn no_lsp_no_notice() {assert!(notices(json!({})).is_empty());assert!(notices(json!({"lsp":[]})).is_empty());}
}
