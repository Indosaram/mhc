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
pub enum EnsuredHost{Reused{pid:u32},Started(StartedHost),HandedOff(crate::host_successor::RunningSuccessor),Refused(&'static str)}
pub async fn ensure_prepared_host(mut prepared:LockedHostEnsure,socket:&str,agent_dir:&std::path::Path,launch:&crate::host_launch::HostLaunch,mut start:HostStartOptions,handoff:crate::host_handoff::HandoffOptions)->std::io::Result<EnsuredHost>{
    use crate::host_decision::{HostDecision,RefuseReason};
    let socket=normalize_socket_path(socket);
    let attached_pid=prepared.registration.as_ref().filter(|owner|owner.socket.as_deref().is_none_or(|registered|registered==socket)).map_or(0,|owner|owner.pid);
    match prepared.decision{
        HostDecision::Reuse{..}=>return Ok(EnsuredHost::Reused{pid:attached_pid}),
        HostDecision::Refuse{reason,..}=>return Ok(EnsuredHost::Refused(match reason{RefuseReason::Protocol=>"protocol",RefuseReason::Capability=>"capability"})),
        HostDecision::Handoff{..}=>{
            prepared.lock.release().map_err(std::io::Error::other)?;
            return match crate::host_handoff::handoff_host(socket,agent_dir,handoff).await?{Ok(successor)=>Ok(EnsuredHost::HandedOff(successor)),Err(_)=>Ok(EnsuredHost::Reused{pid:attached_pid})};
        },
        HostDecision::Start{..}=>{},
        HostDecision::Fallback{..}=>return Ok(EnsuredHost::Reused{pid:attached_pid}),
    }
    if let Some(reason)=start_refusal(&prepared,socket).await?{return Ok(EnsuredHost::Refused(reason));}
    let owner=crate::host_daemon_registration::proven_owner(prepared.registration.as_ref(),socket);
    let stranded=owner.is_some_and(|owner|!crate::host_daemon_registration::written_by_this_process(owner.writer.as_ref()));
    if let Some(owner)=owner&&!stranded{stop_managed_host(owner,std::time::Duration::from_secs(5)).await?;}
    if stranded{
        start.settings.generation=owner.map_or(0.,|owner|owner.generation+1.);
        start.env.insert(crate::protocol_identity::HOST_GENERATION_ENV.into(),start.settings.generation.to_string());
    }else if prepared.registration.as_ref().is_some_and(|owner|owner.socket.as_deref().is_none_or(|registered|registered==socket)){
        crate::host_daemon_registration::clear_host_registration(&prepared.paths)?;
    }
    let started=start_host(&prepared,socket,launch,start).await?;
    prepared.lock.release().map_err(std::io::Error::other)?;
    Ok(EnsuredHost::Started(started))
}
pub async fn stop_managed_host(owner:&crate::host_daemon_registration::RegisteredHost,timeout:std::time::Duration)->std::io::Result<()>{
    for signal in [rustix::process::Signal::TERM,rustix::process::Signal::KILL]{
        let owned=owner.process_start_time.as_ref().is_some_and(|recorded|crate::host_reservations::read_process_start_time(owner.pid).as_ref()==Some(recorded));
        if !owned{return if crate::host_reservations::process_is_live(owner.pid){Err(std::io::Error::other("host process identity is unknown"))}else{Ok(())};}
        crate::host_stop::signal_generation(owner.pid,signal).map_err(std::io::Error::from)?;
        let deadline=tokio::time::Instant::now()+if signal==rustix::process::Signal::TERM{timeout}else{std::time::Duration::from_secs(2)};
        loop{
            if !crate::host_reservations::process_is_live(owner.pid)||owner.process_start_time.as_ref().is_some_and(|recorded|crate::host_reservations::read_process_start_time(owner.pid).as_ref().is_some_and(|current|current!=recorded)){return Ok(());}
            if tokio::time::Instant::now()>=deadline{break;}
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    }
    Err(std::io::Error::other("host remained alive after SIGKILL"))
}
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
/// Whether `start_host` failed because the spawned host never came up (senpi `ensureHost` refuses
/// rather than reporting an opaque IO error for a host it could not bring online).
fn is_host_start_failure(error:&std::io::Error)->bool{
    let message=error.to_string();
    message.starts_with("RPC socket host exited before ready")||message.starts_with("RPC socket host did not become ready")
}

/** Whether a newer build may take the socket over from the running host (senpi `HostUpgradePolicy`). */
#[derive(Debug,Clone,Copy,PartialEq,Eq,Default)]
pub enum HostUpgradePolicy { #[default] Never, IfEngineDiffers }
/** One ensure request: the endpoint, what to launch, and the lifecycle policy to record. */
#[derive(Debug,Clone)]
pub struct EnsureHostOptions {
    pub socket:String,
    pub agent_dir:Option<std::path::PathBuf>,
    pub policy:Option<crate::host_launch_spec::HostLifecyclePolicyInput>,
    pub host_args:Vec<String>,
    /** Extensions the spawned host loads; they form its launch profile. */
    pub extensions:Vec<String>,
    pub env:HashMap<String,Option<String>>,
    pub upgrade:HostUpgradePolicy,
    pub readiness_ms:u64,
}
impl Default for EnsureHostOptions { fn default()->Self{Self{socket:String::new(),agent_dir:None,policy:None,host_args:vec![],extensions:vec![],env:HashMap::new(),upgrade:HostUpgradePolicy::Never,readiness_ms:10_000}} }
/** The host a client ended up attached to, and whether this call started it. */
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct EnsuredHostInfo { pub pid:u32,pub socket:String,pub reused:bool }
#[derive(Debug,thiserror::Error)]
pub enum EnsureHostError {
    #[error("host ensure refused: {0}")] Refused(&'static str),
    #[error(transparent)] Io(#[from]std::io::Error),
}
/** The supervisor argv an ensure hands its spawned host, matching `parse_supervisor_args`. */
pub fn ensure_supervisor_args(socket:&str,agent_dir:&str,host_args:&[String])->Vec<String>{
    ["--socket".into(),socket.into(),"--agent-dir".into(),agent_dir.into()].into_iter().chain(host_args.iter().cloned()).collect()
}
/**
 * Ensure a host serves `options.socket`, starting one when nothing compatible is there.
 *
 * Composes the locked ownership preflight with the spawn/handoff orchestration so a client
 * gets one call that either returns a live endpoint or refuses without touching somebody
 * else's host (senpi `ensureHost`).
 */
pub async fn ensure_host(options:EnsureHostOptions)->Result<EnsuredHostInfo,EnsureHostError>{
    let socket=normalize_socket_path(&options.socket).to_owned();
    let agent_dir=options.agent_dir.clone().unwrap_or_else(||std::path::PathBuf::from(maho_core::config::get_agent_dir()));
    let daemon_dir=crate::host_daemon_paths::create_host_daemon_paths(&socket,&agent_dir).dir;
    let instance_id=crate::protocol_identity::resolve_instance_id(std::env::var(crate::protocol_identity::HOST_INSTANCE_ID_ENV).ok().as_deref());
    let generation=0u64;
    let build=maho_core::engine_build_identity::engine_build_identity().clone();
    let launch_profile=crate::protocol_identity::launch_profile_from_core(crate::host_protocol_info::RpcLaunchProfileCore{extensions:options.extensions.clone(),multi_session:true,session_runtime:crate::host_protocol_info::SessionRuntimeKind::InProcess}).ok();
    let client=crate::host_decision::HostDecisionClient{protocol_version:crate::host_decision::HOST_PROTOCOL_VERSION,required_capabilities:crate::host_decision::REQUIRED_HOST_CAPABILITIES.iter().map(|value|(*value).into()).collect(),identity:build,launch_profile:launch_profile.clone(),started_by_us:false,platform:if cfg!(windows){"win32"}else{std::env::consts::OS}.into()};
    let decision_policy=match options.upgrade{HostUpgradePolicy::Never=>crate::host_decision::HostDecisionPolicy::Never,HostUpgradePolicy::IfEngineDiffers=>crate::host_decision::HostDecisionPolicy::Upgrade};
    let prepared=prepare_ensure_host(&socket,&agent_dir,client,decision_policy).await.map_err(|error|EnsureHostError::Io(std::io::Error::other(error.to_string())))?;
    let supervisor_args=ensure_supervisor_args(&socket,&agent_dir.to_string_lossy(),&options.host_args);
    let launch=crate::host_launch::default_host_launch(&supervisor_args)?;
    let lifecycle=options.policy.clone().unwrap_or_default();
    let host_policy=crate::host_lifecycle::HostLifecyclePolicy{cold_start:lifecycle.cold_start.clone().unwrap_or_else(||"transient".into()),idle_exit_ms:lifecycle.idle_exit_ms.unwrap_or(crate::host_lifecycle::DEFAULT_HOST_IDLE_EXIT_MS)};
    let settings=crate::host_daemon_state::HostDaemonSettings{socket:socket.clone(),capabilities:crate::host_launch::PINNED_HOST_CLIENT_CAPABILITIES.iter().map(|value|(*value).into()).collect(),cold_start:host_policy.cold_start.clone(),idle_exit_ms:host_policy.idle_exit_ms,generation:generation as f64,instance_id:instance_id.clone()};
    let env=host_env(std::env::vars().collect(),&options.env,&agent_dir.to_string_lossy(),&daemon_dir.to_string_lossy(),&instance_id,generation);
    let launch_profile_id=launch_profile.map_or_else(String::new,|profile|profile.profile_id);
    let start=HostStartOptions{env:env.clone(),settings,launch_profile_id:launch_profile_id.clone(),timeout:std::time::Duration::from_millis(options.readiness_ms)};
    let handoff=crate::host_handoff::HandoffOptions{host_args:supervisor_args,policy:Some(host_policy),env:options.env.clone(),launch_profile_id,readiness_ms:options.readiness_ms};
    let outcome=match ensure_prepared_host(prepared,&socket,&agent_dir,&launch,start,handoff).await{
        Ok(outcome)=>outcome,
        // `start_host` reports a host that never became ready with these messages; that is the
        // ensure's refusal (it could not produce the endpoint it was asked for), not a caller-facing
        // IO error. Any other failure - a missing launch binary, a permission or address conflict -
        // stays an IO error so a real resource problem is never masked as a refusal.
        Err(error) if is_host_start_failure(&error)=>return Err(EnsureHostError::Refused("host_unavailable")),
        Err(error)=>return Err(EnsureHostError::Io(error)),
    };
    match outcome{
        EnsuredHost::Refused(reason)=>Err(EnsureHostError::Refused(reason)),
        EnsuredHost::Reused{pid}=>Ok(EnsuredHostInfo{pid,socket,reused:true}),
        EnsuredHost::Started(host)=>Ok(EnsuredHostInfo{pid:host.child.id().unwrap_or_default(),socket,reused:false}),
        EnsuredHost::HandedOff(host)=>Ok(EnsuredHostInfo{pid:host.child.id().unwrap_or_default(),socket,reused:false}),
    }
}
pub fn host_env(mut env:HashMap<String,String>,overrides:&HashMap<String,Option<String>>,agent_dir:&str,daemon_dir:&str,instance_id:&str,generation:u64)->HashMap<String,String>{
    for(key,value)in overrides{if let Some(value)=value{env.insert(key.clone(),value.clone());}else{env.remove(key);}}
    env.insert(maho_core::config::env_agent_dir_var(),agent_dir.into());
    env.insert(crate::custom_capability::RPC_CLIENT_CAPABILITIES_ENV.into(),crate::host_launch::PINNED_HOST_CLIENT_CAPABILITIES.join(","));
    env.insert(crate::protocol_identity::HOST_INSTANCE_ID_ENV.into(),instance_id.into());
    env.insert(crate::protocol_identity::HOST_GENERATION_ENV.into(),generation.to_string());
    env.insert(crate::host_daemon_paths::HOST_DAEMON_DIR_ENV.into(),daemon_dir.into());
    env
}
#[cfg(test)]mod tests{use super::*;
#[test]fn ensure_supervisor_argv_names_endpoint_and_agent_dir(){assert_eq!(ensure_supervisor_args("/tmp/s","/agent",&["--permission".into(),"read".into()]),vec!["--socket","/tmp/s","--agent-dir","/agent","--permission","read"]);assert_eq!(ensure_supervisor_args("unix:///tmp/s","/agent",&[]),vec!["--socket","unix:///tmp/s","--agent-dir","/agent"]);}
#[test]fn ensure_options_default_to_never_upgrade_and_bounded_readiness(){let options=EnsureHostOptions::default();assert_eq!(options.upgrade,HostUpgradePolicy::Never);assert_eq!(options.readiness_ms,10_000);}
#[tokio::test]async fn ensure_refuses_a_foreign_writer_without_starting(){let temp=tempfile::tempdir().unwrap();let socket=temp.path().join("absent.sock");let outcome=ensure_host(EnsureHostOptions{socket:socket.to_string_lossy().into_owned(),agent_dir:Some(temp.path().to_path_buf()),readiness_ms:200,..Default::default()}).await;assert!(outcome.is_ok()||matches!(outcome,Err(EnsureHostError::Refused(_))));}
#[test]fn fixed_host_wiring_wins_over_nullable_overrides(){let key=crate::protocol_identity::HOST_INSTANCE_ID_ENV;let env=host_env(HashMap::from([("REMOVE".into(),"x".into())]),&HashMap::from([(key.into(),None),("REMOVE".into(),None)]),"/agent","/daemon","fresh",0);assert_eq!(env[key],"fresh");assert_eq!(env[&maho_core::config::env_agent_dir_var()],"/agent");assert!(!env.contains_key("REMOVE"));}#[test]fn logical_socket_scheme_and_child_profile_args_match(){assert_eq!(normalize_socket_path("unix:///tmp/s"),"/tmp/s");assert_eq!(normalize_socket_path("/tmp/s"),"/tmp/s");assert_eq!(host_child_argv(&["--permission".into(),"read".into()]),vec!["--mode","rpc","--multi-session","--permission","read"]);}}
