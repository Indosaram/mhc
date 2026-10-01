pub fn signal_generation(pid:u32,signal:rustix::process::Signal)->Result<bool,rustix::io::Errno>{
    let Some(pid)=i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw)else{return Ok(false);};
    match rustix::process::kill_process(pid,signal){Ok(())=>Ok(true),Err(error)if error==rustix::io::Errno::SRCH=>Ok(false),Err(error)=>Err(error)}
}
#[derive(Debug,PartialEq,Eq)]pub enum StopDecision{Drain,Stop,Refuse(&'static str)}
pub fn stop_decision(owner_proven:bool,drain:bool,force:bool,windows:bool,handoff_capable:bool,host_answered:bool,sessions:Option<usize>)->StopDecision{
    if !owner_proven{return StopDecision::Refuse("unknown_owner");}
    if drain{return if windows||!handoff_capable{StopDecision::Refuse("drain_unsupported")}else{StopDecision::Drain};}
    if !force&&host_answered&&sessions.is_some_and(|sessions|sessions>0){return StopDecision::Refuse("sessions_live");}
    StopDecision::Stop
}
#[cfg(test)]mod tests{use super::*;#[test]fn guards_owner_before_signal_and_live_sessions_before_hard_stop(){assert_eq!(stop_decision(false,true,true,false,true,true,Some(1)),StopDecision::Refuse("unknown_owner"));assert_eq!(stop_decision(true,true,false,false,false,true,Some(0)),StopDecision::Refuse("drain_unsupported"));assert_eq!(stop_decision(true,false,false,false,true,true,Some(1)),StopDecision::Refuse("sessions_live"));assert_eq!(stop_decision(true,true,false,false,true,true,Some(1)),StopDecision::Drain);assert_eq!(stop_decision(true,false,true,false,true,true,Some(1)),StopDecision::Stop);}#[test]fn process_gone_is_already_stopped(){assert!(!signal_generation(0,rustix::process::Signal::TERM).unwrap());}}
