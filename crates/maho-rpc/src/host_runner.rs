use crate::host_protocol_info::HostProtocolInfo;
pub const HOST_EXIT_OK:i32=0;
pub const HOST_EXIT_ERROR:i32=1;
pub const HOST_EXIT_USAGE:i32=2;
pub const HOST_EXIT_REFUSED:i32=3;
pub const HOST_EXIT_FALLBACK:i32=4;
pub struct HostOutcome{pub exit_code:i32,pub payload:serde_json::Value}
pub struct EnsureRequest{pub client:crate::host_decision::HostDecisionClient,pub policy:crate::host_decision::HostDecisionPolicy,pub launch:crate::host_launch::HostLaunch,pub start:crate::host_ensure::HostStartOptions,pub handoff:crate::host_handoff::HandoffOptions,pub env_keys:Vec<String>}
pub async fn ensure_outcome(socket:&str,agent_dir:&std::path::Path,request:EnsureRequest)->std::io::Result<HostOutcome>{
    let before=crate::host_probe::probe_protocol_info(socket,10000).await;
    if request.policy==crate::host_decision::HostDecisionPolicy::Fallback&&let crate::host_decision::HostDecision::Fallback{reason,..}=crate::host_decision::decide_host_action(&request.client,before.as_ref(),request.policy){
        let reason=match reason{crate::host_decision::FallbackReason::Capability=>"capability",crate::host_decision::FallbackReason::EngineMismatch=>"engine_mismatch"};
        return Ok(refusal(true,before.as_ref(),serde_json::Map::from_iter([("reason".into(),reason.into()),("socket".into(),socket.into())])));
    }
    let prepared=crate::host_ensure::prepare_ensure_host(socket,agent_dir,request.client,request.policy).await.map_err(std::io::Error::other)?;
    let ensured=crate::host_ensure::ensure_prepared_host(prepared,socket,agent_dir,&request.launch,request.start,request.handoff).await?;
    let(pid,reused)=match ensured{
        crate::host_ensure::EnsuredHost::Refused(reason)=>return Ok(refusal(false,before.as_ref(),serde_json::Map::from_iter([("reason".into(),reason.into()),("socket".into(),socket.into())]))),
        crate::host_ensure::EnsuredHost::Reused{pid}=>(pid,true),
        crate::host_ensure::EnsuredHost::Started(host)=>(host.child.id().unwrap_or_default(),false),
        crate::host_ensure::EnsuredHost::HandedOff(host)=>(host.child.id().unwrap_or_default(),false),
    };
    let host=crate::host_probe::probe_protocol_info(socket,10000).await;
    if !reused{let paths=crate::host_daemon_paths::create_host_daemon_paths(socket,agent_dir);crate::host_daemon_env::write_daemon_env_keys(&paths,&request.env_keys).map_err(|error|error.source)?;}
    let mut payload=identity_payload(socket,pid,host.as_ref());payload["action"]=ensure_action(reused,before.as_ref(),host.as_ref()).into();payload["reused"]=reused.into();
    Ok(HostOutcome{exit_code:HOST_EXIT_OK,payload})
}
pub async fn handoff_outcome(socket:&str,agent_dir:&std::path::Path,options:crate::host_handoff::HandoffOptions,env_keys:&[String])->std::io::Result<HostOutcome>{
    let before=crate::host_probe::probe_protocol_info(socket,10000).await;
    match crate::host_handoff::handoff_host(socket,agent_dir,options).await?{
        Err(result)=>Ok(refusal(false,before.as_ref(),serde_json::Map::from_iter([("reason".into(),upgrade_refusal(result.reason).into()),("socket".into(),socket.into()),("detail".into(),result.reason.into()),("upgradeable".into(),result.upgradeable.into())]))),
        Ok(successor)=>{
            let paths=crate::host_daemon_paths::create_host_daemon_paths(socket,agent_dir);
            crate::host_daemon_env::write_daemon_env_keys(&paths,env_keys).map_err(|error|error.source)?;
            let pid=successor.child.id().unwrap_or_default();
            let mut payload=identity_payload(socket,pid,Some(&successor.protocol));payload["action"]="handoff".into();payload["reused"]=false.into();
            Ok(HostOutcome{exit_code:HOST_EXIT_OK,payload})
        }
    }
}
pub fn identity_payload(socket:&str,pid:u32,host:Option<&HostProtocolInfo>)->serde_json::Value{
    let client=crate::host_decision::HostDecisionClient{protocol_version:crate::host_decision::HOST_PROTOCOL_VERSION,required_capabilities:crate::host_decision::REQUIRED_HOST_CAPABILITIES.iter().map(|value|(*value).into()).collect(),identity:maho_core::engine_build_identity::engine_build_identity().clone(),launch_profile:None,started_by_us:false,platform:if cfg!(windows){"win32"}else{std::env::consts::OS}.into()};
    let decision=serde_json::to_value(crate::host_decision::decide_host_action(&client,host,crate::host_decision::HostDecisionPolicy::Upgrade)).expect("decision has no failing serializer");
    serde_json::json!({"socket":socket,"pid":pid,"instanceId":host.and_then(|host|host.instance_id.as_ref()),"generation":host.and_then(|host|host.generation),"engineVersion":host.and_then(|host|host.engine_version.as_ref()),"engineOrdinal":host.and_then(|host|host.engine_ordinal),"capabilities":host.map_or(&[][..],|host|host.capabilities.as_slice()),"launchProfileId":host.and_then(|host|host.launch_profile.as_ref()).map(|profile|&profile.profile_id),"upgradeable":decision["upgradeable"]})
}
pub async fn status_outcome(socket:&str,agent_dir:&std::path::Path,include_workers:bool)->std::io::Result<HostOutcome>{let payload=crate::host_status::read_host_status(socket,agent_dir,include_workers).await?;Ok(HostOutcome{exit_code:if payload["reachable"]==true{HOST_EXIT_OK}else{HOST_EXIT_REFUSED},payload})}
pub async fn stop_outcome(socket:&str,agent_dir:&std::path::Path,drain:bool,force:bool)->std::io::Result<HostOutcome>{
    let host=crate::host_probe::probe_protocol_info(socket,10000).await;
    let sessions=crate::host_status::read_session_counts(socket,true).await;
    let reason=if !drain&&!force&&sessions.foreign_attached+sessions.foreign_retained>0{Some("sessions_live")}else{
        match crate::host_stop::stop_host(socket,agent_dir,drain,force,10000).await?{
            crate::host_stop::StopHostResult::Refuse{reason}=>Some(reason),
            result=>{let(action,pid)=match result{crate::host_stop::StopHostResult::Drained{pid}=>("drained",pid),crate::host_stop::StopHostResult::Stopped{pid}=>("stopped",pid),crate::host_stop::StopHostResult::Refuse{..}=>unreachable!()};return Ok(HostOutcome{exit_code:HOST_EXIT_OK,payload:serde_json::json!({"action":action,"socket":socket,"pid":pid,"sessions":sessions})});}
        }
    };
    Ok(refusal(false,host.as_ref(),serde_json::Map::from_iter([("reason".into(),reason.into()),("socket".into(),socket.into()),("sessions".into(),serde_json::to_value(sessions).map_err(std::io::Error::other)?)])))
}
pub fn ensure_action(reused:bool,before:Option<&HostProtocolInfo>,after:Option<&HostProtocolInfo>)->&'static str{if reused{"reuse"}else if before.is_some_and(|before|before.instance_id.as_ref()!=after.and_then(|after|after.instance_id.as_ref())){"handoff"}else{"start"}}
pub fn upgrade_refusal(reason:&str)->&str{if reason=="handoff_unsupported"{"upgrade_unsupported"}else{reason}}
pub fn refusal(fallback:bool,host:Option<&HostProtocolInfo>,mut body:serde_json::Map<String,serde_json::Value>)->HostOutcome{let kind=if fallback{"fallback"}else{"refuse"};let mut payload=serde_json::Map::new();payload.insert("action".into(),kind.into());payload.append(&mut body);payload.insert("host".into(),crate::host_status::host_summary(host));HostOutcome{exit_code:if fallback{HOST_EXIT_FALLBACK}else{HOST_EXIT_REFUSED},payload:payload.into()}}
#[cfg(test)]mod tests{use super::*;#[test]fn ensure_labels_distinguish_identity_replacement_from_start(){let mut before=crate::host_protocol_info::parse_host_protocol_info(&serde_json::json!({"serverVersion":"1","capabilities":[]})).unwrap();assert_eq!(ensure_action(false,None,None),"start");assert_eq!(ensure_action(false,Some(&before),None),"start");before.instance_id=Some("old".into());assert_eq!(ensure_action(false,Some(&before),None),"handoff");assert_eq!(ensure_action(true,Some(&before),None),"reuse");assert_eq!(ensure_action(false,Some(&before),Some(&before)),"start");}#[test]fn refusal_preserves_body_and_exit_code(){let outcome=refusal(true,None,serde_json::Map::from_iter([("reason".into(),"missing".into())]));assert_eq!(outcome.exit_code,4);assert_eq!(outcome.payload["action"],"fallback");assert!(outcome.payload["host"].is_null());assert_eq!(upgrade_refusal("handoff_unsupported"),"upgrade_unsupported");assert_eq!(upgrade_refusal("unknown_owner"),"unknown_owner");}}
