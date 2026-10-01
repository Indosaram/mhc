pub fn signal_generation(pid:u32,signal:rustix::process::Signal)->Result<bool,rustix::io::Errno>{
    let Some(pid)=i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw)else{return Ok(false);};
    match rustix::process::kill_process(pid,signal){Ok(())=>Ok(true),Err(error)if error==rustix::io::Errno::SRCH=>Ok(false),Err(error)=>Err(error)}
}
#[derive(Debug,PartialEq,Eq)]pub enum StopDecision{Drain,Stop,Refuse(&'static str)}
#[derive(Debug,PartialEq,Eq)]pub enum StopHostResult{Drained{pid:u32},Stopped{pid:u32},Refuse{reason:&'static str}}
pub async fn stop_host(socket:&str,agent_dir:&std::path::Path,drain:bool,force:bool,timeout_ms:u64)->std::io::Result<StopHostResult>{
    let paths=crate::host_daemon_paths::create_host_daemon_paths(socket,agent_dir);
    let registered=crate::host_daemon_registration::read_host_registration(&paths)?;
    let Some(owner)=crate::host_daemon_registration::proven_owner(registered.as_ref(),socket)else{return Ok(StopHostResult::Refuse{reason:"unknown_owner"});};
    let host=crate::host_probe::probe_protocol_info(socket,timeout_ms).await;
    let handoff=host.as_ref().is_some_and(|host|host.capabilities.iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY));
    let sessions=if drain{None}else{crate::host_probe::probe_session_count(socket,timeout_ms).await};
    match stop_decision(true,drain,force,false,handoff,host.is_some(),sessions){
        StopDecision::Refuse(reason)=>Ok(StopHostResult::Refuse{reason}),
        StopDecision::Drain=>{signal_generation(owner.pid,rustix::process::Signal::USR1).map_err(std::io::Error::from)?;Ok(StopHostResult::Drained{pid:owner.pid})},
        StopDecision::Stop=>{signal_generation(owner.pid,rustix::process::Signal::TERM).map_err(std::io::Error::from)?;crate::host_daemon_registration::release_generation(&paths,&owner.instance_id,owner.pid)?;Ok(StopHostResult::Stopped{pid:owner.pid})},
    }
}
pub fn stop_decision(owner_proven:bool,drain:bool,force:bool,windows:bool,handoff_capable:bool,host_answered:bool,sessions:Option<usize>)->StopDecision{
    if !owner_proven{return StopDecision::Refuse("unknown_owner");}
    if drain{return if windows||!handoff_capable{StopDecision::Refuse("drain_unsupported")}else{StopDecision::Drain};}
    if !force&&host_answered&&sessions.is_some_and(|sessions|sessions>0){return StopDecision::Refuse("sessions_live");}
    StopDecision::Stop
}
#[cfg(test)]mod tests{use super::*;#[test]fn guards_owner_before_signal_and_live_sessions_before_hard_stop(){assert_eq!(stop_decision(false,true,true,false,true,true,Some(1)),StopDecision::Refuse("unknown_owner"));assert_eq!(stop_decision(true,true,false,false,false,true,Some(0)),StopDecision::Refuse("drain_unsupported"));assert_eq!(stop_decision(true,false,false,false,true,true,Some(1)),StopDecision::Refuse("sessions_live"));assert_eq!(stop_decision(true,true,false,false,true,true,Some(1)),StopDecision::Drain);assert_eq!(stop_decision(true,false,true,false,true,true,Some(1)),StopDecision::Stop);}#[test]fn process_gone_is_already_stopped(){assert!(!signal_generation(0,rustix::process::Signal::TERM).unwrap());}}
