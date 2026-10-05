use crate::host_protocol_info::HostProtocolInfo;
#[derive(Debug,PartialEq,Eq)]pub struct HandoffRefusal{pub reason:&'static str,pub upgradeable:bool}
pub struct HandoffContext{pub paths:crate::host_daemon_paths::HostDaemonPaths,pub host:HostProtocolInfo,pub owner:crate::host_daemon_registration::RegisteredHost}
pub struct HandoffOptions{pub host_args:Vec<String>,pub policy:Option<crate::host_lifecycle::HostLifecyclePolicy>,pub env:std::collections::HashMap<String,Option<String>>,pub launch_profile_id:String,pub readiness_ms:u64}
pub async fn handoff_host(socket:&str,agent_dir:&std::path::Path,options:HandoffOptions)->std::io::Result<Result<crate::host_successor::RunningSuccessor,HandoffRefusal>>{
    let socket=crate::host_ensure::normalize_socket_path(socket);
    let paths=crate::host_daemon_paths::create_host_daemon_paths(socket,agent_dir);
    crate::host_daemon_paths::create_daemon_directories(&paths).map_err(|error|error.source)?;
    let mut lock=crate::ownership_safe_lock::acquire_ownership_safe_lock(&paths.lock_file,Default::default()).await.map_err(std::io::Error::other)?;
    let result=async{
        let context=match prepare_handoff(socket,agent_dir,if cfg!(windows){"win32"}else{std::env::consts::OS}).await?{Ok(context)=>context,Err(refusal)=>return Ok(Err(refusal))};
        let generation=context.owner.generation as u64+1;
        let instance_id=crate::protocol_identity::resolve_instance_id(None);
        let prepared=match crate::host_successor::prepare_successor(socket,&context.paths,generation,&instance_id,options.policy.as_ref(),&options.host_args){
            Ok(prepared)=>prepared,
            Err(crate::host_successor::PrepareSuccessorError::SocketPathTooLong(_))=>return Ok(Err(HandoffRefusal{reason:"socket_path_too_long",upgradeable:true})),
            Err(crate::host_successor::PrepareSuccessorError::SocketReplaced)=>return Ok(Err(HandoffRefusal{reason:"socket_replaced",upgradeable:true})),
            Err(error)=>return Err(std::io::Error::other(error)),
        };
        let launch=crate::host_launch::default_host_launch(&prepared.argv)?;
        let env=crate::host_successor::successor_env(std::env::vars().collect(),generation,&instance_id,&context.paths.dir.to_string_lossy(),Some(&agent_dir.to_string_lossy()),&options.env);
        crate::host_successor::start_successor(&context,socket,prepared.replaced,crate::host_successor::SuccessorLaunchOptions{launch,env,instance_id,generation,launch_profile_id:options.launch_profile_id,readiness_ms:options.readiness_ms}).await
    }.await;
    lock.release().map_err(std::io::Error::other)?;result
}
pub async fn prepare_handoff(socket:&str,agent_dir:&std::path::Path,platform:&str)->std::io::Result<Result<HandoffContext,HandoffRefusal>>{
    if platform=="win32"{return Ok(Err(HandoffRefusal{reason:"upgrade_unsupported",upgradeable:false}));}
    let paths=crate::host_daemon_paths::create_host_daemon_paths(socket,agent_dir);
    crate::host_daemon_paths::create_daemon_directories(&paths).map_err(|error|error.source)?;
    let host=crate::host_probe::probe_protocol_info(socket,10000).await;
    if let Some(refusal)=handoff_refusal(platform,host.as_ref(),true){return Ok(Err(refusal));}
    let registered=crate::host_daemon_registration::read_host_registration(&paths)?;
    if crate::host_daemon_registration::proven_owner(registered.as_ref(),socket).is_none(){return Ok(Err(HandoffRefusal{reason:"unknown_owner",upgradeable:true}));}
    Ok(Ok(HandoffContext{paths,host:host.expect("preflight checked host"),owner:registered.expect("preflight proved owner")}))
}
pub fn handoff_refusal(platform:&str,host:Option<&HostProtocolInfo>,owner_proven:bool)->Option<HandoffRefusal>{
    let reason=if platform=="win32"{Some(("upgrade_unsupported",false))}else if let Some(host)=host{if !host.capabilities.iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY){Some(("handoff_unsupported",false))}else if !owner_proven{Some(("unknown_owner",true))}else{None}}else{Some(("no_host",false))};
    reason.map(|(reason,upgradeable)|HandoffRefusal{reason,upgradeable})
}
#[cfg(test)]mod tests{use super::*;#[test]fn platform_and_capability_refusals_precede_owner_check(){let mut host=crate::host_protocol_info::parse_host_protocol_info(&serde_json::json!({"protocolVersion":1,"serverVersion":"1","capabilities":[]})).unwrap();assert_eq!(handoff_refusal("win32",None,false).unwrap().reason,"upgrade_unsupported");assert_eq!(handoff_refusal("linux",None,false).unwrap().reason,"no_host");assert_eq!(handoff_refusal("linux",Some(&host),false).unwrap().reason,"handoff_unsupported");host.capabilities.push(crate::host_decision::GENERATION_HANDOFF_CAPABILITY.into());assert_eq!(handoff_refusal("linux",Some(&host),false),Some(HandoffRefusal{reason:"unknown_owner",upgradeable:true}));assert!(handoff_refusal("linux",Some(&host),true).is_none());}}
