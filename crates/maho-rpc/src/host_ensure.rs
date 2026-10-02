use std::collections::HashMap;
pub fn normalize_socket_path(value:&str)->&str{value.strip_prefix("unix://").unwrap_or(value)}
pub fn host_child_argv(args:&[String])->Vec<String>{["--mode","rpc","--multi-session"].into_iter().map(str::to_owned).chain(args.iter().cloned()).collect()}
pub fn host_env(mut env:HashMap<String,String>,overrides:&HashMap<String,Option<String>>,agent_dir:&str,daemon_dir:&str,instance_id:&str,generation:u64)->HashMap<String,String>{
    for(key,value)in overrides{if let Some(value)=value{env.insert(key.clone(),value.clone());}else{env.remove(key);}}
    env.insert(maho_core::config::env_agent_dir_var(),agent_dir.into());
    env.insert(crate::custom_capability::RPC_CLIENT_CAPABILITIES_ENV.into(),crate::host_launch::PINNED_HOST_CLIENT_CAPABILITIES.join(","));
    env.insert(crate::protocol_identity::HOST_INSTANCE_ID_ENV.into(),instance_id.into());
    env.insert(crate::protocol_identity::HOST_GENERATION_ENV.into(),generation.to_string());
    env.insert(crate::host_daemon_paths::HOST_DAEMON_DIR_ENV.into(),daemon_dir.into());
    env
}
#[cfg(test)]mod tests{use super::*;#[test]fn fixed_host_wiring_wins_over_nullable_overrides(){let key=crate::protocol_identity::HOST_INSTANCE_ID_ENV;let env=host_env(HashMap::from([("REMOVE".into(),"x".into())]),&HashMap::from([(key.into(),None),("REMOVE".into(),None)]),"/agent","/daemon","fresh",0);assert_eq!(env[key],"fresh");assert_eq!(env[&maho_core::config::env_agent_dir_var()],"/agent");assert!(!env.contains_key("REMOVE"));}#[test]fn logical_socket_scheme_and_child_profile_args_match(){assert_eq!(normalize_socket_path("unix:///tmp/s"),"/tmp/s");assert_eq!(normalize_socket_path("/tmp/s"),"/tmp/s");assert_eq!(host_child_argv(&["--permission".into(),"read".into()]),vec!["--mode","rpc","--multi-session","--permission","read"]);}}
