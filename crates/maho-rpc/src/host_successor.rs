use crate::{host_protocol_info::HostProtocolInfo,socket_ownership::{SocketFileIdentity,stat_socket_identity}};
pub const DEFAULT_HANDOFF_READINESS_MS:u64=30_000;
#[derive(Debug,PartialEq,Eq)]pub struct SuccessorPreparation{pub bind_socket:String,pub replaced:SocketFileIdentity,pub argv:Vec<String>}
#[derive(Debug,thiserror::Error)]pub enum PrepareSuccessorError{
    #[error("socket_path_too_long: {0}")]SocketPathTooLong(String),
    #[error("socket_replaced")]SocketReplaced,
    #[error(transparent)]Io(#[from]std::io::Error),
    #[error(transparent)]State(#[from]crate::host_daemon_paths::HostDaemonStateError),
}
pub fn prepare_successor(socket:&str,paths:&crate::host_daemon_paths::HostDaemonPaths,generation:u64,instance_id:&str,policy:Option<&crate::host_lifecycle::HostLifecyclePolicy>,host_args:&[String])->Result<SuccessorPreparation,PrepareSuccessorError>{
    let bind_socket=crate::socket_ownership::generation_bind_path(socket,generation);
    if bind_socket.len()>crate::socket_ownership::MAX_SOCKET_PATH_BYTES{return Err(PrepareSuccessorError::SocketPathTooLong(bind_socket));}
    let replaced=stat_socket_identity(std::path::Path::new(socket))?.ok_or(PrepareSuccessorError::SocketReplaced)?;
    let running=crate::host_daemon_state::read_host_settings(paths)?;
    let cold_start=policy.map(|policy|policy.cold_start.clone()).or_else(||running.as_ref().and_then(|running|running.get("coldStart")).and_then(serde_json::Value::as_str).map(str::to_owned)).unwrap_or_else(||"transient".into());
    let idle_exit_ms=policy.map(|policy|policy.idle_exit_ms).or_else(||running.as_ref().and_then(|running|running.get("idleExitMs")).and_then(serde_json::Value::as_f64)).unwrap_or(crate::host_lifecycle::DEFAULT_HOST_IDLE_EXIT_MS);
    crate::host_daemon_state::write_host_settings(paths,&crate::host_daemon_state::HostDaemonSettings{socket:socket.into(),capabilities:crate::host_launch::PINNED_HOST_CLIENT_CAPABILITIES.iter().map(|value|(*value).into()).collect(),cold_start,idle_exit_ms,generation:generation as f64,instance_id:instance_id.into()})?;
    let argv=["--socket".into(),socket.into(),"--bind".into(),bind_socket.clone(),"--replace".into(),format!("{}:{}",replaced.dev,replaced.ino)].into_iter().chain(host_args.iter().cloned()).collect();
    Ok(SuccessorPreparation{bind_socket,replaced,argv})
}
pub fn successor_env(mut env:std::collections::HashMap<String,String>,generation:u64,instance_id:&str,daemon_dir:&str,agent_dir:Option<&str>,overrides:&std::collections::HashMap<String,Option<String>>)->std::collections::HashMap<String,String>{
    env.insert(crate::protocol_identity::HOST_GENERATION_ENV.into(),generation.to_string());
    env.insert(crate::protocol_identity::HOST_INSTANCE_ID_ENV.into(),instance_id.into());
    env.insert(crate::host_daemon_paths::HOST_DAEMON_DIR_ENV.into(),daemon_dir.into());
    env.insert(crate::custom_capability::RPC_CLIENT_CAPABILITIES_ENV.into(),crate::host_launch::PINNED_HOST_CLIENT_CAPABILITIES.join(","));
    if let Some(agent_dir)=agent_dir.filter(|dir|!dir.is_empty()){env.insert(maho_core::config::env_agent_dir_var(),agent_dir.into());}
    for(key,value)in overrides{if let Some(value)=value{env.insert(key.clone(),value.clone());}else{env.remove(key);}}
    env
}
#[cfg(test)]mod env_tests{use super::*;use std::collections::HashMap;#[test]fn successor_identity_replaces_parent_and_nullable_overrides_apply_last(){let parent=HashMap::from([(crate::protocol_identity::HOST_INSTANCE_ID_ENV.into(),"old".into()),("DROP".into(),"old".into())]);let env=successor_env(parent,2,"new","/daemon",None,&HashMap::from([("DROP".into(),None),("X".into(),Some("y".into()))]));assert_eq!(env[crate::protocol_identity::HOST_INSTANCE_ID_ENV],"new");assert_eq!(env[crate::protocol_identity::HOST_GENERATION_ENV],"2");assert_eq!(env[crate::custom_capability::RPC_CLIENT_CAPABILITIES_ENV],"extension_events,custom_unsupported");assert!(!env.contains_key("DROP"));assert_eq!(env["X"],"y");}}
pub fn successor_answered(previous:&HostProtocolInfo,answer:&HostProtocolInfo)->bool{answer.instance_id.as_ref().is_some_and(|id|Some(id)!=previous.instance_id.as_ref())}
pub fn abort_reason(socket:&std::path::Path,replaced:SocketFileIdentity)->&'static str{if stat_socket_identity(socket).ok().flatten()==Some(replaced){"successor_unavailable"}else{"socket_replaced"}}
#[cfg(test)]mod tests{use super::*;#[test]fn public_answer_must_name_different_instance(){let previous=crate::host_protocol_info::parse_host_protocol_info(&serde_json::json!({"protocolVersion":1,"serverVersion":"1","capabilities":[],"instanceId":"old"})).unwrap();let mut answer=previous.clone();assert!(!successor_answered(&previous,&answer));answer.instance_id=None;assert!(!successor_answered(&previous,&answer));answer.instance_id=Some("new".into());assert!(successor_answered(&previous,&answer));}#[test]fn abort_reason_observes_real_socket_entry(){let temp=tempfile::tempdir().unwrap();let path=temp.path().join("s");let socket=std::os::unix::net::UnixListener::bind(&path).unwrap();let identity=stat_socket_identity(&path).unwrap().unwrap();assert_eq!(abort_reason(&path,identity),"successor_unavailable");drop(socket);std::fs::remove_file(&path).unwrap();assert_eq!(abort_reason(&path,identity),"socket_replaced");}}
