use std::collections::BTreeMap;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub struct ReapToken {pub generation:u64,pub token:u64,pub delay_ms:i64}
#[derive(Clone,Copy)]
pub struct Candidate {pub generation:u64,pub last_used_at:i64}
#[derive(Debug,PartialEq,Eq)]
pub enum ReapAction {Ignore,Expire,Rearm(ReapToken)}
pub struct ReapScheduler {pub idle_ttl_ms:i64,scheduled:BTreeMap<String,ReapToken>,next_token:u64}
impl ReapScheduler {
    pub fn new(idle_ttl_ms:i64)->Self {Self {idle_ttl_ms,scheduled:BTreeMap::new(),next_token:0}}
    pub fn arm(&mut self,session:&str,generation:u64,delay_ms:Option<i64>)->ReapToken {
        self.next_token+=1;let token=ReapToken {generation,token:self.next_token,delay_ms:delay_ms.unwrap_or(self.idle_ttl_ms)};self.scheduled.insert(session.into(),token);token
    }
    pub fn cancel(&mut self,session:&str,generation:u64)->bool {
        if self.scheduled.get(session).is_some_and(|token|token.generation==generation) {self.scheduled.remove(session);true}else {false}
    }
    pub fn fire(&mut self,session:&str,token:ReapToken,candidate:Option<Candidate>,now:i64)->ReapAction {
        if self.scheduled.get(session)!=Some(&token) {return ReapAction::Ignore;}self.scheduled.remove(session);
        let Some(candidate)=candidate.filter(|candidate|candidate.generation==token.generation) else {return ReapAction::Ignore;};
        let remaining=candidate.last_used_at+self.idle_ttl_ms-now;
        if remaining<=0 {ReapAction::Expire}else {ReapAction::Rearm(self.arm(session,token.generation,Some(remaining)))}
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_callbacks_never_expire_current_generation() {
        let mut scheduler=ReapScheduler::new(100);let old=scheduler.arm("s",1,None);let current=scheduler.arm("s",2,None);assert_eq!(scheduler.fire("s",old,Some(Candidate {generation:1,last_used_at:0}),200),ReapAction::Ignore);assert!(!scheduler.cancel("s",1));assert_eq!(scheduler.fire("s",current,Some(Candidate {generation:2,last_used_at:0}),100),ReapAction::Expire);
    }
    #[test]
    fn touched_entry_rearms_for_exact_remaining_time() {
        let mut scheduler=ReapScheduler::new(100);let first=scheduler.arm("s",1,None);let action=scheduler.fire("s",first,Some(Candidate {generation:1,last_used_at:80}),100);let ReapAction::Rearm(next)=action else {panic!("rearm");};assert_eq!(next.delay_ms,80);assert_eq!(scheduler.fire("s",next,Some(Candidate {generation:1,last_used_at:80}),180),ReapAction::Expire);assert_eq!(scheduler.fire("s",next,None,180),ReapAction::Ignore);
    }
}
