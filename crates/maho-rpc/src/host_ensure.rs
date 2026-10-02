use std::collections::HashMap;
pub struct LockedHostEnsure{
    pub paths:crate::host_daemon_paths::HostDaemonPaths,
    pub lock:crate::ownership_safe_lock::OwnershipSafeLock,
    pub registration:Option<crate::host_daemon_registration::RegisteredHost>,
    pub protocol:Option<crate::host_protocol_info::HostProtocolInfo>,
    pub decision:crate::host_decision::HostDecision,
}
pub struct StartedHost{pub child:tokio::process::Child,pub protocol:crate::host_protocol_info::HostProtocolInfo,pub instance_id:String}
pub struct HostStartOptions{pub env:HashMap<String,String>,pub settings:crate::host_daemon_state::HostDaemonSettings,pub launch_profile_id:String,pub timeout:std::time::Duration}
pub async fn start_host(prepared:&LockedHostEnsure,socket:&str,launch:&crate::host_launch::HostLaunch,options:HostStartOptions)->std::io::Result<StartedHost>{
    let HostStartOptions{env,settings,launch_profile_id,timeout}=options;
    use std::os::unix::fs::OpenOptionsExt;
    let socket=normalize_socket_path(socket);
    crate::host_daemon_state::write_host_settings(&prepared.paths,&settings).map_err(|error|error.source)?;
    let stderr=std::fs::OpenOptions::new().create(true).write(true).truncate(settings.generation==0.).append(settings.generation!=0.).mode(0o600).open(&prepared.paths.stderr_log)?;
    let mut child=tokio::process::Command::new(&launch.command).args(&launch.args).env_clear().envs(env).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(stderr).process_group(0).spawn()?;
    let pid=child.id().ok_or_else(||std::io::Error::other("RPC socket host has no process identity"))?;
    let result=async{
        let registration=crate::host_daemon_registration::HostRegistration{pid,process_start_time:crate::host_reservations::read_process_start_time(pid),socket:socket.into(),instance_id:settings.instance_id.clone(),generation:settings.generation,launch_profile_id};
        crate::host_daemon_registration::write_host_registration(&prepared.paths,&registration).map_err(|error|error.source)?;
        let deadline=tokio::time::Instant::now()+timeout;
        loop{
            if let Some(status)=child.try_wait()?{return Err(std::io::Error::other(format!("RPC socket host exited before ready ({status})")));}
            let remaining=deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero(){return Err(std::io::Error::new(std::io::ErrorKind::TimedOut,"RPC socket host did not become ready"));}
            let probe_ms=remaining.as_millis().min(1000) as u64;
            if let Some(protocol)=crate::host_probe::probe_protocol_info(socket,probe_ms).await&&protocol.instance_id.as_deref()==Some(settings.instance_id.as_str()){return Ok(protocol);}
            tokio::time::sleep(std::time::Duration::from_millis(50).min(deadline.saturating_duration_since(tokio::time::Instant::now()))).await;
        }
    }.await;
    match result{
        Ok(protocol)=>Ok(StartedHost{child,protocol,instance_id:settings.instance_id}),
        Err(error)=>{
            let _=child.kill().await;let _=child.wait().await;
            crate::host_daemon_registration::release_generation(&prepared.paths,&settings.instance_id,pid)?;
            Err(error)
        }
    }
}
pub async fn start_refusal(prepared:&LockedHostEnsure,socket:&str)->std::io::Result<Option<&'static str>>{
    let socket=normalize_socket_path(socket);
    if let Some(registered)=crate::host_daemon_registration::proven_owner(prepared.registration.as_ref(),socket){
        if crate::host_daemon_registration::written_by_this_process(registered.writer.as_ref()){
            if crate::host_probe::probe_socket_reachable(socket,2000).await{return Ok(Some("host_busy"));}
        }else if public_endpoint_accepts(socket).await{return Ok(Some("foreign_writer"));}
    }
    let legacy=crate::host_daemon_state::read_file_or_undefined(&prepared.paths.legacy_pid_file)?;
    if let Some(legacy)=crate::host_daemon_state::parse_json(legacy.as_deref())&&let Some(pid)=legacy.get("pid").and_then(serde_json::Value::as_u64).and_then(|pid|u32::try_from(pid).ok()){
        let recorded=legacy.get("processStartTime").and_then(serde_json::Value::as_str);
        if crate::host_reservations::process_is_live(pid)&&recorded.is_none_or(|recorded|crate::host_reservations::read_process_start_time(pid).as_deref()==Some(recorded)){return Ok(Some("legacy_host"));}
    }
    Ok(None)
}
pub async fn prepare_ensure_host(socket:&str,agent_dir:&std::path::Path,mut client:crate::host_decision::HostDecisionClient,policy:crate::host_decision::HostDecisionPolicy)->Result<LockedHostEnsure,crate::ownership_safe_lock::LockError>{
    let socket=normalize_socket_path(socket);
    let paths=crate::host_daemon_paths::create_host_daemon_paths(socket,agent_dir);
    crate::host_daemon_paths::create_daemon_directories(&paths).map_err(|error|error.source)?;
    let lock=crate::ownership_safe_lock::acquire_ownership_safe_lock(&paths.lock_file,Default::default()).await?;
    let registration=crate::host_daemon_registration::read_host_registration(&paths)?;
    let registered_here=registration.as_ref().filter(|registered|registered.socket.as_deref().is_none_or(|registered|registered==socket));
    client.started_by_us=registered_here.is_some_and(|registered|crate::host_daemon_registration::written_by_this_process(registered.writer.as_ref()));
    let protocol=crate::host_probe::probe_protocol_info(socket,2000).await;
    let policy=if policy==crate::host_decision::HostDecisionPolicy::Upgrade{policy}else{crate::host_decision::HostDecisionPolicy::Never};
    let mut decision=crate::host_decision::decide_host_action(&client,protocol.as_ref(),policy);
    if matches!(decision,crate::host_decision::HostDecision::Fallback{..}){decision=crate::host_decision::HostDecision::Reuse{reason:crate::host_decision::ReuseReason::Compatible,upgradeable:false,warning:None};}
    Ok(LockedHostEnsure{paths,lock,registration,protocol,decision})
}
pub fn normalize_socket_path(value:&str)->&str{value.strip_prefix("unix://").unwrap_or(value)}
pub async fn public_endpoint_accepts(socket:&str)->bool{
    if socket.starts_with('\0'){return true;}
    match crate::socket_ownership::stat_socket_identity(std::path::Path::new(socket)){
        Ok(None)=>false,
        Err(_)=>true,
        Ok(Some(_))=>crate::host_probe::probe_socket_reachable(socket,2000).await
    }
}
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
