use crate::host_protocol_info::HostProtocolInfo;
pub const HOST_EXIT_OK:i32=0;
pub const HOST_EXIT_ERROR:i32=1;
pub const HOST_EXIT_USAGE:i32=2;
pub const HOST_EXIT_REFUSED:i32=3;
pub const HOST_EXIT_FALLBACK:i32=4;
pub struct HostOutcome{pub exit_code:i32,pub payload:serde_json::Value}
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
