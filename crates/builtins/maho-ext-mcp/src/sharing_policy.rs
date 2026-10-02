use std::collections::BTreeMap;
use sha2::{Digest,Sha256};
use serde_json::{Value,json};
use crate::config_schema::{McpServerConfig,Transport};

fn has_session_values(config:&McpServerConfig,cwd:Option<&str>)->bool {
    let variables=regex::Regex::new(r"(?i)(?:cwd|session|^PWD$|^OLDPWD$)").unwrap_or_else(|error|panic!("constant regex: {error}"));
    let templates=regex::Regex::new(r"(?i)\$\{[^}]*(?:cwd|session|PWD)[^}]*\}").unwrap_or_else(|error|panic!("constant regex: {error}"));
    let values=config.command.iter().chain(config.args.iter().flatten()).chain(config.env.iter().flat_map(|env|env.values()));
    config.env.as_ref().is_some_and(|env|env.keys().any(|key|variables.is_match(key))) ||
        values.clone().chain(config.cwd.iter()).any(|value|templates.is_match(value)) ||
        cwd.is_some_and(|cwd|values.clone().any(|value|value.contains(cwd)))
}
pub fn inherit_mcp_sharing_scope(source:&McpServerConfig,target:&mut McpServerConfig) {
    target.session_dependent|=source.session_dependent || has_session_values(source,None);
}
pub fn shareable(config:&McpServerConfig,cwd:Option<&str>)->bool {
    config.transport==Some(Transport::Http) || (!config.session_dependent && !has_session_values(config,cwd))
}
fn stable_json(value:&Value)->String {
    match value {
        Value::Object(map)=>{
            let mut keys=map.keys().collect::<Vec<_>>();keys.sort();
            format!("{{{}}}",keys.into_iter().map(|key|format!("{}:{}",json!(key),stable_json(&map[key]))).collect::<Vec<_>>().join(","))
        }
        Value::Array(values)=>format!("[{}]",values.iter().map(stable_json).collect::<Vec<_>>().join(",")),
        _=>value.to_string(),
    }
}
pub fn shared_mcp_key(name:&str,config:&McpServerConfig,env:Option<&BTreeMap<String,String>>,agent_dir:&str,process_cwd:&str)->String {
    let stdio=config.transport==Some(Transport::Stdio);
    let mut merged=env.cloned().unwrap_or_default();merged.extend(config.env.clone().unwrap_or_default());
    let bearer=config.bearer_token_env.as_ref().and_then(|name|env.and_then(|env|env.get(name)).cloned().or_else(||std::env::var(name).ok()));
    let identity=json!({"agentDir":agent_dir,"name":name,"type":config.transport,"url":if stdio {None}else{config.url.as_ref()},
        "command":if stdio {config.command.as_ref()}else{None},"args":if stdio {config.args.as_ref()}else{None},
        "cwd":if stdio {Some(config.cwd.as_deref().unwrap_or(process_cwd))}else{None},"env":if stdio {Some(merged)}else{None},
        "headers":config.headers,"auth":config.auth,"oauth":config.oauth,"bearer":bearer,"connectTimeoutMs":config.connect_timeout_ms,"logLevel":config.log_level});
    format!("{:x}",Sha256::digest(stable_json(&identity)))
}
