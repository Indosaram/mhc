use crate::socket_ownership::EndpointOwnership;
pub const SUPERSESSION_POLL_MS:u64=1000;
pub const ABSENT_CONFIRMATIONS:u32=3;
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum EndpointLoss{Replaced,Absent}
#[derive(Default)]
pub struct SupersessionWatcher{absent_polls:u32,lost:bool}
impl SupersessionWatcher{
    pub fn observe(&mut self,ownership:EndpointOwnership,settled:bool)->Option<EndpointLoss>{
        if settled||self.lost{return None;}
        let loss=match ownership{
            EndpointOwnership::Replaced=>Some(EndpointLoss::Replaced),
            EndpointOwnership::Absent=>{self.absent_polls+=1;(self.absent_polls>=ABSENT_CONFIRMATIONS).then_some(EndpointLoss::Absent)},
            EndpointOwnership::Held|EndpointOwnership::Unknown=>{self.absent_polls=0;None}
        };
        if loss.is_some(){self.lost=true;}loss
    }
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test] fn unknown_resets_consecutive_absence_and_loss_latches(){let mut watcher=SupersessionWatcher::default();assert_eq!(watcher.observe(EndpointOwnership::Absent,false),None);watcher.observe(EndpointOwnership::Unknown,false);for _ in 0..2{assert_eq!(watcher.observe(EndpointOwnership::Absent,false),None);}assert_eq!(watcher.observe(EndpointOwnership::Absent,false),Some(EndpointLoss::Absent));assert_eq!(watcher.observe(EndpointOwnership::Replaced,false),None);}
    #[test] fn settled_observation_is_ignored(){let mut watcher=SupersessionWatcher::default();assert_eq!(watcher.observe(EndpointOwnership::Replaced,true),None);assert_eq!(watcher.observe(EndpointOwnership::Replaced,false),Some(EndpointLoss::Replaced));}
}
