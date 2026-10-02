use crate::host_protocol_info::HostProtocolInfo;
#[derive(Debug,PartialEq,Eq)]pub struct HandoffRefusal{pub reason:&'static str,pub upgradeable:bool}
pub struct HandoffContext{pub paths:crate::host_daemon_paths::HostDaemonPaths,pub host:HostProtocolInfo,pub owner:crate::host_daemon_registration::RegisteredHost}
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
