use crate::{host_protocol_info::HostProtocolInfo,socket_ownership::{SocketFileIdentity,stat_socket_identity}};
pub const DEFAULT_HANDOFF_READINESS_MS:u64=30_000;
pub struct SuccessorLaunchOptions{pub launch:crate::host_launch::HostLaunch,pub env:std::collections::HashMap<String,String>,pub instance_id:String,pub generation:u64,pub launch_profile_id:String,pub readiness_ms:u64}
pub struct RunningSuccessor{pub child:tokio::process::Child,pub protocol:HostProtocolInfo}
pub async fn start_successor(context:&crate::host_handoff::HandoffContext,socket:&str,replaced:SocketFileIdentity,options:SuccessorLaunchOptions)->std::io::Result<Result<RunningSuccessor,crate::host_handoff::HandoffRefusal>>{
    use std::os::unix::fs::OpenOptionsExt;
    let stderr=std::fs::OpenOptions::new().create(true).append(true).mode(0o600).open(&context.paths.stderr_log)?;
    let mut child=tokio::process::Command::new(&options.launch.command).args(&options.launch.args).env_clear().envs(options.env).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(stderr).process_group(0).spawn()?;
    let pid=child.id().ok_or_else(||std::io::Error::other("successor has no process identity"))?;
    let result:std::io::Result<Option<HostProtocolInfo>>=async{
        let deadline=tokio::time::Instant::now()+std::time::Duration::from_millis(options.readiness_ms);
        loop{
            if let Some(answer)=crate::host_probe::probe_protocol_info(socket,2000).await&&successor_answered(&context.host,&answer){
                crate::host_daemon_registration::write_host_registration(&context.paths,&crate::host_daemon_registration::HostRegistration{pid,process_start_time:crate::host_reservations::read_process_start_time(pid),socket:socket.into(),instance_id:options.instance_id,generation:options.generation as f64,launch_profile_id:options.launch_profile_id}).map_err(|error|error.source)?;
                crate::host_stop::signal_generation(context.owner.pid,rustix::process::Signal::USR1).map_err(std::io::Error::from)?;
                return Ok(Some(answer));
            }
            if child.try_wait()?.is_some()||tokio::time::Instant::now()>=deadline{return Ok(None);}
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }.await;
    match result{
        Ok(Some(protocol))=>Ok(Ok(RunningSuccessor{child,protocol})),
        result=>{
            let _=child.kill().await;let _=child.wait().await;
            let reason=if result.is_err(){"successor_unavailable"}else{abort_reason(std::path::Path::new(socket),replaced)};
            Ok(Err(crate::host_handoff::HandoffRefusal{reason,upgradeable:true}))
        }
    }
}
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
