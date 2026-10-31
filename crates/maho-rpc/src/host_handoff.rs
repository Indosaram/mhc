use crate::host_protocol_info::HostProtocolInfo;
#[derive(Debug,PartialEq,Eq)]pub struct HandoffRefusal{pub reason:&'static str,pub upgradeable:bool,pub detail:Option<String>}
/// What a successor launch acts on: the endpoint, the running host, and the predecessor's proven identity -
/// `None` when nothing proves it, and then it is never signalled (senpi `startSuccessor` context).
pub struct HandoffContext{pub paths:crate::host_daemon_paths::HostDaemonPaths,pub host:HostProtocolInfo,pub owner:Option<crate::host_daemon_registration::RegisteredHost>}
const HANDOFF_PROBE_TIMEOUT_MS:u64=10_000;
/// The longest a handoff holds the ensure lock: its probe of the running host, the two session counts an
/// unregistered host gets (before the swap and the recount after it), then the successor's start.
pub const HANDOFF_LOCK_HOLD_MS:u64=HANDOFF_PROBE_TIMEOUT_MS+2*crate::host_handoff_unregistered::SESSION_COUNT_TIMEOUT_MS+crate::host_successor::SUCCESSOR_START_BUDGET_MS;
/// One handoff request. `launch` is how the successor is launched from supervisor argv: `None` is THIS process's
/// runtime; a running host performing an idle handover passes the runtime of the CLI that asked for it (its
/// `command` and leading `args`, the supervisor argv appended).
pub struct HandoffOptions{pub host_args:Vec<String>,pub policy:Option<crate::host_lifecycle::HostLifecyclePolicy>,pub env:std::collections::HashMap<String,Option<String>>,pub launch_profile_id:String,pub readiness_ms:u64,pub launch:Option<crate::host_launch::HostLaunch>}
pub async fn handoff_host(socket:&str,agent_dir:&std::path::Path,options:HandoffOptions)->std::io::Result<Result<crate::host_successor::RunningSuccessor,HandoffRefusal>>{
    let socket=crate::host_ensure::normalize_socket_path(socket);
    let paths=crate::host_daemon_paths::create_host_daemon_paths(socket,agent_dir);
    crate::host_daemon_paths::create_daemon_directories(&paths).map_err(|error|error.source)?;
    let mut lock=crate::ownership_safe_lock::acquire_ownership_safe_lock(&paths.lock_file,Default::default()).await.map_err(std::io::Error::other)?;
    let result=handoff_host_locked(socket,agent_dir,options).await;
    lock.release().map_err(std::io::Error::other)?;result
}
/// The handoff itself, for a caller that already holds this socket's ensure lock: an ensure's upgrade (senpi
/// `handoffHostLocked`). A host no registration proves goes through `handoff_unregistered_host`.
pub async fn handoff_host_locked(socket:&str,agent_dir:&std::path::Path,options:HandoffOptions)->std::io::Result<Result<crate::host_successor::RunningSuccessor,HandoffRefusal>>{
    let socket=crate::host_ensure::normalize_socket_path(socket);
    let context=match prepare_handoff(socket,agent_dir,if cfg!(windows){"win32"}else{std::env::consts::OS}).await?{Ok(context)=>context,Err(refusal)=>return Ok(Err(refusal))};
    if context.owner.is_none(){return crate::host_handoff_unregistered::handoff_unregistered_host(socket,agent_dir,options,context).await;}
    launch_successor(socket,agent_dir,options,&context,None).await
}
/// Prepares and starts the successor generation for `context` (shared by the registered and unregistered paths).
pub async fn launch_successor(socket:&str,agent_dir:&std::path::Path,options:HandoffOptions,context:&HandoffContext,drain_gate:Option<crate::host_successor::DrainGate>)->std::io::Result<Result<crate::host_successor::RunningSuccessor,HandoffRefusal>>{
    let generation=context.host.generation.map_or(0,|generation|generation as u64)+1;
    let instance_id=crate::protocol_identity::resolve_instance_id(None);
    let boot_settings=crate::host_daemon_state::read_file_or_undefined(&context.paths.settings_file)?;
    let prepared=match crate::host_successor::prepare_successor(socket,&context.paths,generation,&instance_id,options.policy.as_ref(),&options.host_args){
        Ok(prepared)=>prepared,
        Err(crate::host_successor::PrepareSuccessorError::SocketPathTooLong(path))=>return Ok(Err(HandoffRefusal{reason:"socket_path_too_long",upgradeable:true,detail:Some(path)})),
        Err(crate::host_successor::PrepareSuccessorError::SocketReplaced)=>return Ok(Err(HandoffRefusal{reason:"socket_replaced",upgradeable:true,detail:None})),
        Err(error)=>return Err(std::io::Error::other(error)),
    };
    let launch=match &options.launch{
        Some(base)=>Ok(crate::host_launch::HostLaunch{command:base.command.clone(),args:base.args.iter().chain(&prepared.argv).cloned().collect()}),
        None=>crate::host_launch::default_host_launch(&prepared.argv),
    };
    let launch=match launch{Ok(launch)=>launch,Err(error)=>{restore_settings(&context.paths,boot_settings.as_deref());return Err(error);}};
    let env=crate::host_successor::successor_env(std::env::vars().collect(),generation,&instance_id,&context.paths.dir.to_string_lossy(),Some(&agent_dir.to_string_lossy()),&options.env);
    crate::host_successor::start_successor_gated(context,socket,prepared.replaced,crate::host_successor::SuccessorLaunchOptions{launch,env,instance_id,generation,launch_profile_id:options.launch_profile_id,readiness_ms:options.readiness_ms},Some(boot_settings),drain_gate).await
}
fn restore_settings(paths:&crate::host_daemon_paths::HostDaemonPaths,boot:Option<&str>){let _=match boot{Some(text)=>std::fs::write(&paths.settings_file,text),None=>std::fs::remove_file(&paths.settings_file)};}
pub async fn prepare_handoff(socket:&str,agent_dir:&std::path::Path,platform:&str)->std::io::Result<Result<HandoffContext,HandoffRefusal>>{
    if platform=="win32"{return Ok(Err(HandoffRefusal{reason:"upgrade_unsupported",upgradeable:false,detail:None}));}
    let paths=crate::host_daemon_paths::create_host_daemon_paths(socket,agent_dir);
    // Created before the agent-dir guard on purpose: realpath needs the caller's dir to exist.
    crate::host_daemon_paths::create_daemon_directories(&paths).map_err(|error|error.source)?;
    let Some(host)=crate::host_probe::probe_protocol_info(socket,HANDOFF_PROBE_TIMEOUT_MS).await else{return Ok(Err(HandoffRefusal{reason:"no_host",upgradeable:false,detail:None}));};
    if let Some(refusal)=handoff_refusal(platform,Some(&host),true){return Ok(Err(refusal));}
    // The running host belongs to another agent dir; only a client of that dir may replace it (#3560).
    if let Some(detail)=crate::host_handoff_agent_dir::foreign_agent_dir_refusal(socket,agent_dir,&host){return Ok(Err(HandoffRefusal{reason:crate::host_handoff_agent_dir::FOREIGN_AGENT_DIR,upgradeable:false,detail:Some(detail)}));}
    // A pointer an earlier prune dropped is given back to the one proven live generation (senpi#1911).
    crate::host_pointer_election::elect_pointer(&paths,socket,true);
    let registered=crate::host_daemon_registration::read_host_registration(&paths)?;
    let owner=crate::host_daemon_registration::proven_owner(registered.as_ref(),socket).cloned();
    Ok(Ok(HandoffContext{paths,host,owner}))
}
pub fn handoff_refusal(platform:&str,host:Option<&HostProtocolInfo>,owner_proven:bool)->Option<HandoffRefusal>{
    let reason=if platform=="win32"{Some(("upgrade_unsupported",false))}else if let Some(host)=host{if !host.capabilities.iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY){Some(("handoff_unsupported",false))}else if !owner_proven{Some(("unknown_owner",true))}else{None}}else{Some(("no_host",false))};
    reason.map(|(reason,upgradeable)|HandoffRefusal{reason,upgradeable,detail:None})
}
#[cfg(test)]mod tests{use super::*;#[test]fn platform_and_capability_refusals_precede_owner_check(){let mut host=crate::host_protocol_info::parse_host_protocol_info(&serde_json::json!({"protocolVersion":1,"serverVersion":"1","capabilities":[]})).unwrap();assert_eq!(handoff_refusal("win32",None,false).unwrap().reason,"upgrade_unsupported");assert_eq!(handoff_refusal("linux",None,false).unwrap().reason,"no_host");assert_eq!(handoff_refusal("linux",Some(&host),false).unwrap().reason,"handoff_unsupported");host.capabilities.push(crate::host_decision::GENERATION_HANDOFF_CAPABILITY.into());assert_eq!(handoff_refusal("linux",Some(&host),false),Some(HandoffRefusal{reason:"unknown_owner",upgradeable:true,detail:None}));assert!(handoff_refusal("linux",Some(&host),true).is_none());}}
