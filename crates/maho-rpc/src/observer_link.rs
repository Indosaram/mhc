pub struct UnknownActivityInput{pub healthy:bool,pub unhealthy_since:Option<f64>,pub now:f64,pub unknown_grace_ms:f64,pub observed_busy:u64}
pub fn active_turns_for_idle_decision(input:&UnknownActivityInput)->u64{
    if input.healthy{return input.observed_busy;}
    let Some(since)=input.unhealthy_since else{return 1;};
    u64::from(input.now-since<input.unknown_grace_ms)
}
#[derive(Default)]
pub struct ObserverLink{current:Option<u64>,healthy:bool,unhealthy_since:Option<f64>,retry_armed:bool}
impl ObserverLink{
    pub fn opened(&mut self,socket:u64){self.current=Some(socket);self.healthy=true;self.unhealthy_since=None;}
    pub const fn healthy(&self)->bool{self.healthy}
    pub const fn unhealthy_since(&self)->Option<f64>{self.unhealthy_since}
    pub fn lost(&mut self,socket:u64,settled:bool,now:f64)->bool{
        if self.current!=Some(socket)||settled{return false;}
        self.healthy=false;self.unhealthy_since.get_or_insert(now);self.current=None;self.arm_retry(settled)
    }
    pub fn arm_retry(&mut self,settled:bool)->bool{if self.retry_armed||settled{return false;}self.retry_armed=true;true}
    pub fn retry_fired(&mut self){self.retry_armed=false;}
    pub fn stop(&mut self){self.retry_armed=false;self.current=None;}
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn unknown_activity_is_busy_only_for_bounded_window(){let mut input=UnknownActivityInput{healthy:false,unhealthy_since:Some(10.),now:109.,unknown_grace_ms:100.,observed_busy:4};assert_eq!(active_turns_for_idle_decision(&input),1);input.now=110.;assert_eq!(active_turns_for_idle_decision(&input),0);input.healthy=true;assert_eq!(active_turns_for_idle_decision(&input),4);}
    #[test]fn duplicate_loss_is_collapsed_and_failed_retry_can_rearm(){let mut link=ObserverLink::default();link.opened(1);assert!(link.lost(1,false,100.));assert!(!link.lost(1,false,101.));assert_eq!(link.unhealthy_since(),Some(100.));link.retry_fired();assert!(link.arm_retry(false));link.opened(2);assert!(link.healthy());assert_eq!(link.unhealthy_since(),None);assert!(!link.lost(1,false,200.));}
}
